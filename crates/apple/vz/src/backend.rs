//! SAFETY: VZVirtualMachine is not thread-safe and must only be accessed from
//! the main thread. This module uses apple_main::on_main to ensure all VM
//! operations happen on the main queue.

use crate::handle::NativeVmHandle;
use crate::vm::{CreateVmConfig, create_pipe, create_vm, get_socket_device_addr, start_vm};
use crate::vsock::VsockBridge;
use async_trait::async_trait;
use capsa_core::{
    BackendCapabilities, BackendVmHandle, DEFAULT_ROOT_DEVICE, Error, HostPlatform,
    HypervisorBackend, KernelCmdline, NetworkMode, Result, VmConfig, macos_cmdline_defaults,
    macos_virtualization_capabilities,
};
use capsa_net::{SocketPairDevice, StackConfig, UserNatStack};
use std::os::fd::{AsRawFd, IntoRawFd};

pub struct NativeVirtualizationBackend {
    capabilities: BackendCapabilities,
}

impl NativeVirtualizationBackend {
    pub fn new() -> Self {
        Self {
            capabilities: macos_virtualization_capabilities(),
        }
    }
}

impl Default for NativeVirtualizationBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HypervisorBackend for NativeVirtualizationBackend {
    fn name(&self) -> &'static str {
        "native-virtualization"
    }

    fn platform(&self) -> HostPlatform {
        HostPlatform::MacOs
    }

    fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    fn is_available(&self) -> bool {
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        static CHECKED: AtomicBool = AtomicBool::new(false);
        static AVAILABLE: AtomicBool = AtomicBool::new(false);

        if CHECKED.load(Ordering::SeqCst) {
            return AVAILABLE.load(Ordering::SeqCst);
        }

        let (tx, rx) = std::sync::mpsc::channel();
        dispatch::Queue::main().exec_async(move || {
            let _ = tx.send(());
        });

        let available = rx.recv_timeout(Duration::from_millis(100)).is_ok();
        AVAILABLE.store(available, Ordering::SeqCst);
        CHECKED.store(true, Ordering::SeqCst);
        available
    }

    async fn start(&self, config: &VmConfig) -> Result<Box<dyn BackendVmHandle>> {
        let (console_input_read, console_input_write, console_output_read, console_output_write) =
            if config.console_enabled {
                let (input_read, input_write) = create_pipe()?;
                let (output_read, output_write) = create_pipe()?;
                (
                    Some(input_read),
                    Some(input_write),
                    Some(output_read),
                    Some(output_write),
                )
            } else {
                (None, None, None, None)
            };

        let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel(1);

        // Create socketpair for UserNat networking.
        // We use into_raw_fd() to transfer ownership because NSFileHandle takes ownership.
        //
        // For Cluster mode, the guest fd is pre-created and passed via cluster_network_fd.
        let (host_net_device, network_guest_fd, user_nat_config) = match &config.network {
            NetworkMode::UserNat(user_nat_config) => {
                let (device, guest_fd) = SocketPairDevice::new().map_err(|e| {
                    Error::StartFailed(format!("Failed to create socketpair: {}", e))
                })?;
                (
                    Some(device),
                    Some(guest_fd.into_raw_fd()),
                    Some(user_nat_config.clone()),
                )
            }
            NetworkMode::Cluster(_) => {
                // For Cluster mode, the guest fd should be pre-created by VmBuilder
                let guest_fd = config.cluster_network_fd.ok_or_else(|| {
                    Error::StartFailed(
                        "Cluster mode requires cluster_network_fd to be set by VmBuilder"
                            .to_string(),
                    )
                })?;
                (None, Some(guest_fd), None)
            }
            _ => (None, None, None),
        };

        let vm_config = CreateVmConfig {
            boot: config.boot.clone(),
            cpus: config.resources.cpus,
            memory_mb: config.resources.memory_mb as u64,
            root_disk: config.root_disk.clone(),
            disks: config.disks.clone(),
            network: config.network.clone(),
            network_guest_fd,
            vsock: config.vsock.clone(),
            console_input_read_fd: console_input_read.as_ref().map(|fd| fd.as_raw_fd()),
            console_output_write_fd: console_output_write.as_ref().map(|fd| fd.as_raw_fd()),
            stop_sender: stop_tx,
        };

        let vsock_ports = config.vsock.ports.clone();

        let (vm_addr, delegate_addr) = apple_main::on_main(move || create_vm(vm_config))
            .await
            .map_err(|e| Error::StartFailed(format!("VM creation failed: {}", e)))?;

        // Set up vsock bridging if ports are configured
        // Step 1: Set up listeners on main thread (ObjC requirement)
        // Step 2: Set up vsock listeners on main thread
        let vsock_setup_result = if !vsock_ports.is_empty() {
            let socket_device_addr =
                apple_main::on_main(move || get_socket_device_addr(vm_addr)).await;

            if socket_device_addr != 0 {
                Some(
                    apple_main::on_main(move || unsafe {
                        let mtm = objc2::MainThreadMarker::new()
                            .expect("vsock bridge must be created on main thread");
                        VsockBridge::setup_listeners(socket_device_addr, vsock_ports, mtm)
                    })
                    .await,
                )
            } else {
                None
            }
        } else {
            None
        };

        // Step 3: Create VsockBridge and spawn bridging tasks (in tokio context)
        let vsock_bridge = vsock_setup_result
            .filter(|r| !r.tasks.is_empty())
            .map(VsockBridge::from_setup_result);

        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        apple_main::on_main(move || {
            start_vm(vm_addr, start_tx);
        })
        .await;

        match tokio::time::timeout(std::time::Duration::from_secs(30), start_rx).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => return Err(Error::StartFailed(format!("VM start failed: {}", e))),
            Ok(Err(_)) => return Err(Error::StartFailed("VM start channel closed".to_string())),
            Err(_) => return Err(Error::StartFailed("VM start timed out".to_string())),
        }

        // Spawn the userspace NAT stack if configured
        let network_task = host_net_device.map(|device| {
            let stack_config = user_nat_config
                .as_ref()
                .map(StackConfig::from)
                .unwrap_or_default();
            let stack = UserNatStack::new(device, stack_config);
            tokio::spawn(async move {
                if let Err(e) = stack.run().await {
                    tracing::error!("UserNat stack error: {:?}", e);
                }
            })
        });

        drop(console_input_read);
        drop(console_output_write);

        Ok(Box::new(NativeVmHandle::new(
            vm_addr,
            delegate_addr,
            console_output_read,
            console_input_write,
            stop_rx,
            vsock_bridge,
            network_task,
        )))
    }

    fn kernel_cmdline_defaults(&self) -> KernelCmdline {
        macos_cmdline_defaults()
    }

    fn default_root_device(&self) -> &str {
        DEFAULT_ROOT_DEVICE
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod backend_construction {
        use super::*;

        #[test]
        fn new_creates_backend_with_correct_capabilities() {
            let backend = NativeVirtualizationBackend::new();
            let caps = backend.capabilities();

            assert!(caps.guest_os.linux);
            assert!(caps.boot_methods.linux_direct);
            assert!(caps.image_formats.raw);
            assert!(!caps.image_formats.qcow2);
            assert!(caps.network_modes.none);
            assert!(caps.network_modes.nat);
            // TODO: Update when virtio-fs is implemented for macOS
            assert!(!caps.share_mechanisms.virtio_fs);
            assert!(!caps.share_mechanisms.virtio_9p);
            assert!(caps.max_cpus.is_none());
            assert!(caps.max_memory_mb.is_none());
        }

        #[test]
        fn default_creates_same_as_new() {
            let backend1 = NativeVirtualizationBackend::new();
            let backend2 = NativeVirtualizationBackend::default();

            let caps1 = backend1.capabilities();
            let caps2 = backend2.capabilities();

            assert_eq!(caps1.guest_os.linux, caps2.guest_os.linux);
            assert_eq!(
                caps1.boot_methods.linux_direct,
                caps2.boot_methods.linux_direct
            );
            assert_eq!(caps1.image_formats.raw, caps2.image_formats.raw);
        }

        #[test]
        fn name_returns_correct_name() {
            let backend = NativeVirtualizationBackend::new();
            assert_eq!(backend.name(), "native-virtualization");
        }
    }

    mod kernel_cmdline {
        use super::*;

        #[test]
        fn defaults_sets_console_to_hvc0() {
            let backend = NativeVirtualizationBackend::new();
            let cmdline = backend.kernel_cmdline_defaults();
            assert_eq!(cmdline.get("console"), Some("hvc0"));
        }

        #[test]
        fn defaults_sets_reboot_to_t() {
            let backend = NativeVirtualizationBackend::new();
            let cmdline = backend.kernel_cmdline_defaults();
            assert_eq!(cmdline.get("reboot"), Some("t"));
        }

        #[test]
        fn defaults_sets_panic_to_minus_one() {
            let backend = NativeVirtualizationBackend::new();
            let cmdline = backend.kernel_cmdline_defaults();
            assert_eq!(cmdline.get("panic"), Some("-1"));
        }

        #[test]
        fn default_root_device_is_vda() {
            let backend = NativeVirtualizationBackend::new();
            assert_eq!(backend.default_root_device(), "/dev/vda");
        }
    }
}
