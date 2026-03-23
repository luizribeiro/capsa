use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};

use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::net::UnixDatagram;

use capsa_net::{bridge_to_switch, GatewayStack, GatewayStackConfig, VirtualSwitch};

use tokio::runtime::{Builder, Runtime};
use tokio::task::JoinHandle;

use crate::{NetworkInterfaceConfig, ResolvedNetworkInterface, VmConfig, VmmLaunchSpec};

const FIRST_GUEST_NET_FD: i32 = 100;

impl VmConfig {
    /// Start the VM in the sandboxed sidecar process.
    pub fn start(&self) -> Result<()> {
        self.validate().context("invalid VM configuration")?;

        let vmm_exe = resolve_vmm_binary()?;
        let spec = vm_sandbox_spec(self, &vmm_exe);

        let mut network_runtime = if self.interfaces.is_empty() {
            None
        } else {
            Some(
                NetworkRuntimeContext::start(&self.interfaces)
                    .context("failed to start network runtime")?,
            )
        };

        let launch_spec = VmmLaunchSpec {
            vm_config: self.clone(),
            resolved_interfaces: network_runtime
                .as_ref()
                .map(NetworkRuntimeContext::resolved_interfaces)
                .unwrap_or_default(),
        };

        let launch_spec_json =
            serde_json::to_string(&launch_spec).context("failed to serialize VMM launch spec")?;
        let child_args = vec!["--launch-spec-json".to_string(), launch_spec_json];

        let child = {
            if let Some(runtime) = network_runtime.as_ref() {
                capsa_sandbox::spawn_sandboxed_with_fds(
                    &vmm_exe,
                    &child_args,
                    &spec,
                    runtime.fd_remaps(),
                )
            } else {
                capsa_sandbox::spawn_sandboxed(&vmm_exe, &child_args, &spec)
            }
        }
        .with_context(|| {
            format!(
                "failed to spawn sandboxed VMM process: {}",
                vmm_exe.display()
            )
        });

        let child = match child {
            Ok(child) => child,
            Err(err) => {
                if let Some(runtime) = network_runtime.take() {
                    runtime
                        .shutdown()
                        .context("failed to shutdown network runtime after spawn failure")?;
                }
                return Err(err);
            }
        };

        if let Some(runtime) = network_runtime.as_mut() {
            // Once spawn returns, child pre-exec fd remapping has already run.
            // We can drop the launcher-owned guest endpoints immediately.
            runtime.release_guest_fds_after_spawn();
        }

        let wait_result = child.wait().context("failed to wait on sandboxed child");

        if let Some(runtime) = network_runtime.take() {
            runtime
                .shutdown()
                .context("failed to shutdown network runtime")?;
        }

        let status = wait_result?;
        if status.success() {
            return Ok(());
        }

        anyhow::bail!("sandboxed VMM process exited with status {status}")
    }
}

#[derive(Debug)]
struct NetworkRuntimeContext {
    runtime: Runtime,
    network: NetworkRuntime,
}

impl NetworkRuntimeContext {
    fn start(interfaces: &[NetworkInterfaceConfig]) -> Result<Self> {
        let runtime = Builder::new_multi_thread()
            .enable_all()
            .build()
            .context("failed to build tokio runtime for networking")?;
        let network = runtime
            .block_on(NetworkRuntime::start(interfaces))
            .context("failed to initialize network runtime")?;

        Ok(Self { runtime, network })
    }

    fn resolved_interfaces(&self) -> Vec<ResolvedNetworkInterface> {
        self.network.resolved_interfaces.clone()
    }

    fn fd_remaps(&self) -> &[capsa_sandbox::FdRemap] {
        &self.network.fd_remaps
    }

    fn release_guest_fds_after_spawn(&mut self) {
        self.network.guest_fds.clear();
    }

    fn shutdown(self) -> Result<()> {
        self.runtime.block_on(self.network.shutdown())
    }
}

#[derive(Debug)]
struct NetworkRuntime {
    resolved_interfaces: Vec<ResolvedNetworkInterface>,
    fd_remaps: Vec<capsa_sandbox::FdRemap>,
    guest_fds: Vec<OwnedFd>,
    bridge_tasks: Vec<JoinHandle<std::io::Result<()>>>,
    gateway_tasks: Vec<JoinHandle<std::io::Result<()>>>,
}

impl NetworkRuntime {
    async fn start(interfaces: &[NetworkInterfaceConfig]) -> Result<Self> {
        ensure!(
            !interfaces.is_empty(),
            "network runtime requires at least one interface"
        );

        let mut resolved_interfaces = Vec::with_capacity(interfaces.len());
        let mut fd_remaps = Vec::with_capacity(interfaces.len());
        let mut guest_fds = Vec::with_capacity(interfaces.len());
        let mut bridge_tasks = Vec::with_capacity(interfaces.len());
        let mut gateway_tasks = Vec::with_capacity(interfaces.len());

        for (index, interface) in interfaces.iter().enumerate() {
            let switch = VirtualSwitch::new();
            let vm_port = switch.create_port().await;
            let gateway_port = switch.create_port().await;

            let (host_fd, guest_fd) = create_unix_dgram_socketpair()
                .with_context(|| format!("failed to create socketpair for interface {index}"))?;

            let guest_target_fd = FIRST_GUEST_NET_FD + index as i32;
            let mac = resolve_interface_mac(index, interface)?;

            let bridge_task = tokio::spawn(async move { bridge_to_switch(host_fd, vm_port).await });
            let gateway = GatewayStack::new(gateway_port, GatewayStackConfig::default()).await;
            let gateway_task = tokio::spawn(async move { gateway.run().await });

            fd_remaps.push(capsa_sandbox::FdRemap {
                source_fd: guest_fd.as_raw_fd(),
                target_fd: guest_target_fd,
            });

            resolved_interfaces.push(ResolvedNetworkInterface {
                mac,
                guest_fd: guest_target_fd,
            });
            guest_fds.push(guest_fd);
            bridge_tasks.push(bridge_task);
            gateway_tasks.push(gateway_task);
        }

        Ok(Self {
            resolved_interfaces,
            fd_remaps,
            guest_fds,
            bridge_tasks,
            gateway_tasks,
        })
    }

    async fn shutdown(self) -> Result<()> {
        for handle in &self.bridge_tasks {
            handle.abort();
        }
        for handle in &self.gateway_tasks {
            handle.abort();
        }

        for handle in self.bridge_tasks {
            let _ = handle.await;
        }
        for handle in self.gateway_tasks {
            let _ = handle.await;
        }

        Ok(())
    }
}

fn create_unix_dgram_socketpair() -> Result<(OwnedFd, OwnedFd)> {
    let (left, right) =
        UnixDatagram::pair().context("failed to create unix datagram socketpair")?;

    let left_raw = left.into_raw_fd();
    let right_raw = right.into_raw_fd();

    // SAFETY: `left_raw` and `right_raw` come from `into_raw_fd`, transferring
    // ownership to the newly created `OwnedFd`s.
    let left_owned = unsafe { OwnedFd::from_raw_fd(left_raw) };
    // SAFETY: same as above for the second socket endpoint.
    let right_owned = unsafe { OwnedFd::from_raw_fd(right_raw) };

    Ok((left_owned, right_owned))
}

fn resolve_interface_mac(index: usize, interface: &NetworkInterfaceConfig) -> Result<[u8; 6]> {
    match interface.mac {
        Some(mac) => {
            ensure!(mac != [0; 6], "interface {index}: MAC address is all zeros");
            Ok(mac)
        }
        None => Ok(generate_mac(index)),
    }
}

fn generate_mac(index: usize) -> [u8; 6] {
    let mut seed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    seed ^= (std::process::id() as u128) << 32;
    seed ^= index as u128;

    let mut mac = [0u8; 6];
    mac[0] = 0x02; // locally administered, unicast
    mac[1] = ((seed >> 8) & 0xff) as u8;
    mac[2] = ((seed >> 16) & 0xff) as u8;
    mac[3] = ((seed >> 24) & 0xff) as u8;
    mac[4] = ((seed >> 32) & 0xff) as u8;
    mac[5] = ((seed >> 40) & 0xff) as u8;

    if mac == [0u8; 6] {
        mac[5] = 1;
    }

    mac
}

fn vm_sandbox_spec(config: &VmConfig, vmm_exe: &Path) -> capsa_sandbox::SandboxSpec {
    let mut spec = capsa_sandbox::SandboxSpec::new().allow_network(false);

    spec.read_only_paths.push(vmm_exe.to_path_buf());

    if let Some(root) = &config.root {
        spec.read_write_paths.push(root.clone());
    }

    if let Some(kernel) = &config.kernel {
        spec.read_only_paths.push(kernel.clone());
    }

    if let Some(initramfs) = &config.initramfs {
        spec.read_only_paths.push(initramfs.clone());
    }

    spec
}

fn resolve_vmm_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CAPSA_VMM_PATH") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    if let Ok(current_exe) = std::env::current_exe() {
        let sibling = current_exe.with_file_name("capsa-vmm");
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    if let Some(in_path) = find_in_path("capsa-vmm") {
        return Ok(in_path);
    }

    anyhow::bail!(
        "unable to locate capsa-vmm sidecar. Build/install it (e.g. `cargo build --bins`) and optionally set CAPSA_VMM_PATH"
    )
}

fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::vm_sandbox_spec;
    use crate::{NetworkInterfaceConfig, VmConfig};

    fn sample_config() -> VmConfig {
        VmConfig {
            root: Some("/tmp/root".into()),
            kernel: Some("/tmp/kernel".into()),
            initramfs: Some("/tmp/initramfs".into()),
            kernel_cmdline: Some("console=ttyS0".to_string()),
            vcpus: 1,
            memory_mib: 512,
            verbosity: 0,
            interfaces: vec![],
        }
    }

    #[test]
    fn vm_sandbox_spec_disables_network_without_interfaces() {
        let config = sample_config();
        let spec = vm_sandbox_spec(&config, std::path::Path::new("/tmp/capsa-vmm"));

        assert!(!spec.allow_network);
    }

    #[test]
    fn start_rejects_multiple_interfaces_before_spawning_sidecar() {
        let mut config = sample_config();
        config.interfaces = vec![
            NetworkInterfaceConfig { mac: None },
            NetworkInterfaceConfig { mac: None },
        ];

        let err = config.start().expect_err("start should fail validation");
        let rendered = format!("{err:#}");
        assert!(rendered.contains("invalid VM configuration"));
        assert!(rendered.contains("multiple network interfaces are not supported yet"));
    }

    #[test]
    fn network_runtime_starts_and_stops_cleanly() {
        let interfaces = vec![NetworkInterfaceConfig { mac: None }];
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime should build");

        let mut network = runtime
            .block_on(super::NetworkRuntime::start(&interfaces))
            .expect("network runtime should start");

        assert_eq!(network.resolved_interfaces.len(), 1);
        assert_eq!(network.fd_remaps.len(), 1);
        assert_eq!(network.guest_fds.len(), 1);

        network.guest_fds.clear();
        runtime
            .block_on(network.shutdown())
            .expect("network runtime should shut down");
    }

    #[test]
    fn network_runtime_preserves_explicit_mac() {
        let mac = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        let interfaces = vec![NetworkInterfaceConfig { mac: Some(mac) }];
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime should build");

        let mut network = runtime
            .block_on(super::NetworkRuntime::start(&interfaces))
            .expect("network runtime should start");

        assert_eq!(network.resolved_interfaces[0].mac, mac);

        network.guest_fds.clear();
        runtime
            .block_on(network.shutdown())
            .expect("network runtime should shut down");
    }

    #[test]
    fn network_runtime_rejects_all_zero_mac() {
        let interfaces = vec![NetworkInterfaceConfig { mac: Some([0; 6]) }];
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime should build");

        let err = runtime
            .block_on(super::NetworkRuntime::start(&interfaces))
            .expect_err("all-zero MAC should be rejected");

        assert!(err.to_string().contains("MAC address is all zeros"));
    }

    #[test]
    fn network_runtime_starts_bridge_task() {
        let interfaces = vec![NetworkInterfaceConfig { mac: None }];
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .expect("runtime should build");

        let mut network = runtime
            .block_on(super::NetworkRuntime::start(&interfaces))
            .expect("network runtime should start");

        runtime.block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        });

        assert_eq!(network.bridge_tasks.len(), 1);
        assert!(!network.bridge_tasks[0].is_finished());

        network.guest_fds.clear();
        runtime
            .block_on(network.shutdown())
            .expect("network runtime should shut down");
    }

    #[test]
    fn release_guest_fds_drops_launcher_copy_after_spawn() {
        let interfaces = vec![NetworkInterfaceConfig { mac: None }];
        let mut ctx = super::NetworkRuntimeContext::start(&interfaces)
            .expect("network runtime context should start");

        assert_eq!(ctx.network.guest_fds.len(), 1);
        ctx.release_guest_fds_after_spawn();
        assert!(ctx.network.guest_fds.is_empty());

        ctx.shutdown().expect("network runtime should shut down");
    }
}
