//! KVM hypervisor backend for capsa.
//!
//! This crate provides a Linux KVM-based hypervisor backend that can boot and run
//! Linux guest VMs using the kernel's KVM virtualization infrastructure.
//!
//! # Features
//!
//! - **Linux Direct Boot**: Boots Linux kernels directly using bzImage format
//! - **Serial Console**: Provides bidirectional console access via emulated 8250 UART
//! - **Multi-CPU Support**: Configurable vCPU count
//!
//! # Requirements
//!
//! - Linux host with KVM support (`/dev/kvm` must be accessible)
//! - x86_64 architecture (currently the only supported architecture)
//!
//! # Example
//!
//! ```no_run
//! use capsa_linux_kvm::KvmBackend;
//! use capsa_core::HypervisorBackend;
//!
//! let backend = KvmBackend::new();
//! if backend.is_available() {
//!     println!("KVM is available");
//! }
//! ```

mod arch;
mod handle;
mod serial;
mod virtio_console;
mod virtio_net;
mod vm;

use async_trait::async_trait;
use capsa_core::{
    BackendCapabilities, BackendVmHandle, BootMethodSupport, DeviceSupport, GuestOsSupport,
    HostPlatform, HypervisorBackend, ImageFormatSupport, KernelCmdline, NetworkModeSupport, Result,
    ShareMechanismSupport, VmConfig,
};
use std::path::Path;

/// KVM hypervisor backend implementation.
///
/// This backend uses Linux's KVM (Kernel-based Virtual Machine) to run guest VMs.
/// It requires `/dev/kvm` to be accessible and currently only supports x86_64.
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
                    uefi: false,
                },
                image_formats: ImageFormatSupport {
                    raw: true,
                    qcow2: false,
                },
                network_modes: NetworkModeSupport {
                    none: true,
                    nat: false,
                    user_nat: true,
                },
                share_mechanisms: ShareMechanismSupport {
                    virtio_fs: false,
                    virtio_9p: false,
                },
                devices: DeviceSupport { vsock: false },
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
        // Use virtio-console for higher throughput
        cmdline.console("hvc0");
        cmdline.arg("reboot", "t");
        cmdline.arg("panic", "-1");
        cmdline.flag("quiet");
        // virtio-console MMIO device
        cmdline.arg(
            "virtio_mmio.device",
            format!(
                "0x{:x}@0x{:x}:{}",
                arch::VIRTIO_MMIO_SIZE,
                arch::VIRTIO_MMIO_BASE,
                arch::VIRTIO_CONSOLE_IRQ
            ),
        );
        // virtio-net MMIO device
        cmdline.arg(
            "virtio_mmio.device",
            format!(
                "0x{:x}@0x{:x}:{}",
                arch::VIRTIO_MMIO_SIZE,
                arch::VIRTIO_NET_MMIO_BASE,
                arch::VIRTIO_NET_IRQ
            ),
        );
        cmdline
    }

    fn default_root_device(&self) -> &str {
        "/dev/vda"
    }
}
