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
//!     // Minimal config - just kernel and initrd (disk is optional)
//!     let config = LinuxDirectBootConfig::new("./bzImage", "./initrd");
//!
//!     let vm = Capsa::vm(config)
//!         .cpus(2)
//!         .memory_mb(2048)
//!         .disk(DiskImage::new("./rootfs.raw"))
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

// TODO: document all public types exported by the capsa library

mod backend;
mod boot;
mod builder;
mod capabilities;
mod config;
mod console;
mod error;
mod handle;
mod pool;
mod types;

// TODO: stop exporting test-utils externally as all it does is expose VMs from test-vms.nix
// there probably is some useful test utilities for capsa that could be helpful
// for users of the library, but these are probably not it (unless things like
// the minimal VMs are bundled with the library)
#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use boot::{CmdlineArg, KernelCmdline, LinuxDirectBootConfig};
pub use builder::LinuxVmBuilder;
pub use capabilities::BackendCapabilities;
pub use config::{Capsa, VmConfig};
pub use console::{ConsoleReader, ConsoleWriter, VmConsole};
pub use error::{Error, Result};
pub use handle::{VmHandle, VmStatus};
pub use pool::{PooledVm, VmPool};
pub use types::{
    ConsoleMode, DiskImage, GuestOs, ImageFormat, MountMode, NetworkMode, ResourceConfig,
    ShareMechanism, SharedDir, Virtio9pConfig, VirtioFsConfig,
};
