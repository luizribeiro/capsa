mod arch;
mod handle;
mod serial;
mod vm;

use async_trait::async_trait;
use capsa_core::{
    BackendCapabilities, BackendVmHandle, BootMethodSupport, DeviceSupport, GuestOsSupport,
    HostPlatform, HypervisorBackend, ImageFormatSupport, KernelCmdline, NetworkModeSupport,
    Result, ShareMechanismSupport, VmConfig,
};
use std::path::Path;

pub struct KvmBackend {
    capabilities: BackendCapabilities,
}

impl KvmBackend {
    pub fn new() -> Self {
        Self {
            capabilities: BackendCapabilities {
                guest_os: GuestOsSupport { linux: true },
                boot_methods: BootMethodSupport {
                    linux_direct: true,
                    uefi: false, // TODO: UEFI boot support
                },
                image_formats: ImageFormatSupport {
                    raw: true,
                    qcow2: false, // TODO: qcow2 support
                },
                network_modes: NetworkModeSupport {
                    none: true,
                    nat: false, // TODO: virtio-net networking
                },
                share_mechanisms: ShareMechanismSupport {
                    virtio_fs: false, // TODO: virtio-fs shares
                    virtio_9p: false, // TODO: virtio-9p shares
                },
                devices: DeviceSupport {
                    vsock: false, // TODO: vsock support
                },
                max_cpus: None,
                max_memory_mb: None,
            },
        }
    }

    fn kvm_available() -> bool {
        Path::new("/dev/kvm").exists()
    }
}

impl Default for KvmBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HypervisorBackend for KvmBackend {
    fn name(&self) -> &'static str {
        "kvm"
    }

    fn platform(&self) -> HostPlatform {
        HostPlatform::Linux
    }

    fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    fn is_available(&self) -> bool {
        Self::kvm_available()
    }

    async fn start(&self, config: &VmConfig) -> Result<Box<dyn BackendVmHandle>> {
        vm::start_vm(config).await
    }

    fn kernel_cmdline_defaults(&self) -> KernelCmdline {
        let mut cmdline = KernelCmdline::new();
        cmdline.console(arch::DEFAULT_CONSOLE);
        cmdline.arg("reboot", "t");
        cmdline.arg("panic", "-1");
        cmdline
    }

    fn default_root_device(&self) -> &str {
        "/dev/vda" // TODO: virtio-blk disk support
    }
}
