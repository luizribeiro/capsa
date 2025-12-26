//! Capsa Sandbox - a VM with guaranteed features.
//!
//! The sandbox uses a capsa-controlled kernel and initrd that provides:
//! - Auto-mounting of shared directories
//! - Main process support via `.run()` or `.oci()`
//! - Guest agent for structured command execution
//! - Known environment with predictable capabilities
//!
//! # Example
//!
//! ```rust,ignore
//! use capsa::Capsa;
//! use capsa_core::MountMode;
//!
//! let vm = Capsa::sandbox()
//!     .share("./workspace", "/mnt", MountMode::ReadWrite)
//!     .cpus(2)
//!     .memory_mb(1024)
//!     .run("/bin/sh", &[])
//!     .build()
//!     .await?;
//!
//! vm.wait_ready().await?;
//! let result = vm.exec("ls /mnt").await?;
//! ```

mod builder;
mod config;

pub use builder::{HasMainProcess, NoMainProcess, SandboxBuilder};
pub use config::CapsaSandboxConfig;

// These will be used when implementing build() and cmdline generation
#[allow(unused_imports)]
pub(crate) use config::{MainProcess, ShareConfig};
