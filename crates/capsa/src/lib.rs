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
//! #[tokio::main]
//! async fn main() -> capsa::Result<()> {
//!     // Minimal config - just kernel and initrd (disk is optional)
//!     let config = LinuxDirectBootConfig::new("./bzImage", "./initrd");
//!
//!     let vm = Capsa::vm(config)
//!         .cpus(2)
//!         .memory_mb(2048)
//!         .disk(DiskImage::new("./rootfs.raw"))  // optional
//!         .share("./workspace", "/workspace", MountMode::ReadWrite)
//!         .console_enabled()
//!         .build()
//!         .await?;
//!
//!     // Interact with console
//!     let console = vm.console().await?;
//!     console.wait_for("login:").await?;
//!     console.write_line("root").await?;
//!
//!     // Graceful shutdown
//!     vm.stop().await?;
//!
//!     Ok(())
//! }
//! ```

mod backend;
mod boot;
mod builder;
mod capabilities;
mod config;
mod console;
mod error;
mod handle;
mod types;

#[cfg(feature = "test-utils")]
pub mod test_utils;

pub use boot::{CmdlineArg, KernelCmdline, LinuxDirectBootConfig};
pub use builder::LinuxVmBuilder;
pub use capabilities::BackendCapabilities;
pub use config::{Capsa, VmConfig};
pub use console::{ConsoleReader, ConsoleWriter, VmConsole};
pub use error::{Error, Result};
pub use handle::{VmHandle, VmStatus};
pub use types::{
    ConsoleMode, DiskImage, GuestOs, ImageFormat, MountMode, NetworkMode, ResourceConfig,
    ShareMechanism, SharedDir, Virtio9pConfig, VirtioFsConfig,
};
