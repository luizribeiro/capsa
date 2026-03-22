use std::path::{Path, PathBuf};

use anyhow::{ensure, Context, Result};
#[cfg(unix)]
use std::os::fd::{AsRawFd, FromRawFd, IntoRawFd, OwnedFd};
use std::os::unix::net::UnixDatagram;

use crate::{LaunchEnvelope, NetworkInterfaceConfig, ResolvedNetworkInterface, VmConfig};

const FIRST_GUEST_NET_FD: i32 = 100;

impl VmConfig {
    /// Start the VM in the sandboxed sidecar process.
    pub fn start(&self) -> Result<()> {
        self.validate().context("invalid VM configuration")?;

        let vmm_exe = resolve_vmm_binary()?;
        let spec = vm_sandbox_spec(self, &vmm_exe);

        #[cfg(unix)]
        let mut prepared_network = prepare_network_endpoints(&self.interfaces)
            .context("failed to prepare network interface endpoints")?;

        let launch_spec = {
            #[cfg(unix)]
            {
                VmmLaunchSpec {
                    vm_config: self.clone(),
                    resolved_interfaces: prepared_network.resolved_interfaces.clone(),
                }
            }

            #[cfg(not(unix))]
            {
                VmmLaunchSpec {
                    vm_config: self.clone(),
                    resolved_interfaces: vec![],
                }
            }
        };
        let envelope_json =
            serde_json::to_string(&envelope).context("failed to serialize launch envelope")?;
        let child_args = vec!["--launch-envelope-json".to_string(), envelope_json];

        let child = {
            #[cfg(unix)]
            {
                if should_spawn_with_fd_remaps(&prepared_network.fd_remaps) {
                    capsa_sandbox::spawn_sandboxed_with_fds(
                        &vmm_exe,
                        &child_args,
                        &spec,
                        &prepared_network.fd_remaps,
                    )
                } else {
                    capsa_sandbox::spawn_sandboxed(&vmm_exe, &child_args, &spec)
                }
            }

            #[cfg(not(unix))]
            {
                capsa_sandbox::spawn_sandboxed(&vmm_exe, &child_args, &spec)
            }
        }
        .with_context(|| {
            format!(
                "failed to spawn sandboxed VMM process: {}",
                vmm_exe.display()
            )
        })?;

        #[cfg(unix)]
        {
            // Once spawn returns, child pre-exec fd remapping has already run.
            // We can drop the launcher-owned guest endpoints immediately.
            prepared_network.guest_fds.clear();
        }

        // Keep host-side network endpoints alive while the sidecar process runs.
        #[cfg(unix)]
        let _host_fds = &prepared_network.host_fds;
        let status = child.wait().context("failed to wait on sandboxed child")?;
        if status.success() {
            return Ok(());
        }

        anyhow::bail!("sandboxed VMM process exited with status {status}")
    }
}

#[cfg(unix)]
#[derive(Debug)]
struct PreparedNetworkEndpoints {
    resolved_interfaces: Vec<ResolvedNetworkInterface>,
    fd_remaps: Vec<capsa_sandbox::FdRemap>,
    host_fds: Vec<OwnedFd>,
    guest_fds: Vec<OwnedFd>,
}

#[cfg(unix)]
fn prepare_network_endpoints(
    interfaces: &[NetworkInterfaceConfig],
) -> Result<PreparedNetworkEndpoints> {
    let mut resolved_interfaces = Vec::with_capacity(interfaces.len());
    let mut fd_remaps = Vec::with_capacity(interfaces.len());
    let mut host_fds = Vec::with_capacity(interfaces.len());
    let mut guest_fds = Vec::with_capacity(interfaces.len());

    for (index, interface) in interfaces.iter().enumerate() {
        let (host_fd, guest_fd) = create_unix_dgram_socketpair()
            .with_context(|| format!("failed to create socketpair for interface {index}"))?;

        let guest_target_fd = FIRST_GUEST_NET_FD + index as i32;
        let mac = resolve_interface_mac(index, interface)?;

        fd_remaps.push(capsa_sandbox::FdRemap {
            source_fd: guest_fd.as_raw_fd(),
            target_fd: guest_target_fd,
        });

        resolved_interfaces.push(ResolvedNetworkInterface {
            mac,
            guest_fd: guest_target_fd,
        });

        host_fds.push(host_fd);
        guest_fds.push(guest_fd);
    }

    Ok(PreparedNetworkEndpoints {
        resolved_interfaces,
        fd_remaps,
        host_fds,
        guest_fds,
    })
}

#[cfg(unix)]
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

#[cfg(unix)]
fn should_spawn_with_fd_remaps(fd_remaps: &[capsa_sandbox::FdRemap]) -> bool {
    !fd_remaps.is_empty()
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
    #[cfg(unix)]
    use std::os::fd::AsRawFd;

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
    fn prepare_network_endpoints_creates_socketpair_and_resolved_interface() {
        let mut config = sample_config();
        config.interfaces.push(NetworkInterfaceConfig { mac: None });

        let prepared = super::prepare_network_endpoints(&config.interfaces)
            .expect("network endpoint preparation should succeed");

        assert_eq!(prepared.resolved_interfaces.len(), 1);
        assert_eq!(prepared.fd_remaps.len(), 1);
        assert_eq!(prepared.host_fds.len(), 1);
        assert_eq!(prepared.guest_fds.len(), 1);

        let resolved = &prepared.resolved_interfaces[0];
        assert_eq!(resolved.guest_fd, super::FIRST_GUEST_NET_FD);
        assert_ne!(resolved.mac, [0u8; 6]);

        let remap = &prepared.fd_remaps[0];
        assert_eq!(remap.target_fd, super::FIRST_GUEST_NET_FD);
        assert_eq!(remap.source_fd, prepared.guest_fds[0].as_raw_fd());
    }

    #[test]
    fn prepare_network_endpoints_preserves_explicit_mac() {
        let mut config = sample_config();
        let mac = [0x02, 0xaa, 0xbb, 0xcc, 0xdd, 0xee];
        config
            .interfaces
            .push(NetworkInterfaceConfig { mac: Some(mac) });

        let prepared = super::prepare_network_endpoints(&config.interfaces)
            .expect("network endpoint preparation should succeed");

        assert_eq!(prepared.resolved_interfaces[0].mac, mac);
    }

    #[test]
    fn prepare_network_endpoints_rejects_all_zero_mac() {
        let mut config = sample_config();
        config
            .interfaces
            .push(NetworkInterfaceConfig { mac: Some([0; 6]) });

        let err = super::prepare_network_endpoints(&config.interfaces)
            .expect_err("all-zero MAC should be rejected");
        assert!(err.to_string().contains("MAC address is all zeros"));
    }

    #[test]
    fn should_spawn_with_fd_remaps_depends_on_remap_presence() {
        assert!(!super::should_spawn_with_fd_remaps(&[]));

        let remaps = [capsa_sandbox::FdRemap {
            source_fd: 9,
            target_fd: 100,
        }];
        assert!(super::should_spawn_with_fd_remaps(&remaps));
    }

    #[test]
    fn host_fd_stays_open_until_prepared_network_is_dropped() {
        use std::os::fd::AsRawFd;

        let mut config = sample_config();
        config.interfaces.push(NetworkInterfaceConfig { mac: None });

        let prepared = super::prepare_network_endpoints(&config.interfaces)
            .expect("network endpoint preparation should succeed");

        let host_fd = prepared.host_fds[0].as_raw_fd();
        assert!(fd_is_open(host_fd));

        drop(prepared);

        assert!(!fd_is_open(host_fd));
    }

    #[cfg(unix)]
    fn fd_is_open(fd: i32) -> bool {
        // SAFETY: `fcntl(F_GETFD)` is read-only and used only to check whether
        // the descriptor currently refers to an open file.
        let rc = unsafe { libc::fcntl(fd, libc::F_GETFD) };
        rc != -1
    }
}
