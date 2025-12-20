//! Capsa - A cross-platform VM runtime library for secure workload isolation.
//!
//! Capsa provides a unified Rust API for running virtual machines across platforms.
//! It abstracts hypervisor differences (vfkit on macOS, cloud-hypervisor on Linux)
//! behind a clean interface.
//!
//! # Example
//!
//! ```rust,no_run
//! use capsa::{Capsa, LinuxDirectBootConfig, DiskImage, MountMode};
//!
//! #[apple_main::main]
//! async fn main() {
//!     let config = LinuxDirectBootConfig::new("./bzImage", "./initrd")
//!         .with_root_disk(DiskImage::new("./rootfs.raw"));
//!
//!     let vm = Capsa::linux(config)
//!         .cpus(2)
//!         .memory_mb(2048)
//!         .share("./workspace", "/workspace", MountMode::ReadWrite)
//!         .console_enabled()
//!         .build()
//!         .await
//!         .unwrap();
//!
//!     let console = vm.console().await.unwrap();
//!     console.wait_for("login:").await.unwrap();
//!     console.write_line("root").await.unwrap();
//!
//!     // Graceful shutdown
//!     vm.stop().await.unwrap();
//! }
//! ```

// TODO: audit which types really should be exposed publicly. things like capabilities
// probably don't have to be public outside of this crate

pub mod backend;
mod builder;
mod config;
mod console;
mod handle;
mod pool;

// TODO: stop exporting test-utils externally as all it does is expose VMs from test-vms.nix
// there probably is some useful test utilities for capsa that could be helpful
// for users of the library, but these are probably not it (unless things like
// the minimal VMs are bundled with the library)
#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use backend::{HostPlatform, HypervisorBackend, available_backends};
pub use builder::LinuxVmBuilder;
pub use config::Capsa;
pub use console::{ConsoleReader, ConsoleWriter, VmConsole};
pub use handle::{VmHandle, VmStatus};
pub use pool::{PooledVm, VmPool};

pub use capsa_core::{
    BackendCapabilities, BootMethodSupport, CmdlineArg, DiskImage, Error, GuestOs, GuestOsSupport,
    ImageFormat, ImageFormatSupport, KernelCmdline, LinuxDirectBootConfig, MountMode, NetworkMode,
    NetworkModeSupport, ResourceConfig, Result, ShareMechanism, ShareMechanismSupport, SharedDir,
    Virtio9pConfig, VirtioFsConfig,
};
