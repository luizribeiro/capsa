//! Cross-platform VM runtime for secure workload isolation.
//!
//! Capsa provides a unified Rust API for creating and managing virtual machines,
//! abstracting hypervisor differences behind a clean interface. It's particularly
//! well-suited for **end-to-end integration testing** where you need to run code
//! in isolated Linux environments.
//!
//! # Quick Start
//!
//! All interaction starts with [`Capsa`]:
//!
//! ```rust,no_run
//! use capsa::{Capsa, LinuxDirectBootConfig, DiskImage};
//!
//! # async fn example() -> capsa::Result<()> {
//! let config = LinuxDirectBootConfig::new("./kernel", "./initrd")
//!     .with_root_disk(DiskImage::new("./rootfs.raw"));
//!
//! let vm = Capsa::linux(config)
//!     .cpus(2)
//!     .memory_mb(1024)
//!     .console_enabled()
//!     .build()
//!     .await?;
//!
//! let console = vm.console().await?;
//! console.wait_for("login:").await?;
//! console.write_line("root").await?;
//!
//! vm.stop().await?;
//! # Ok(())
//! # }
//! ```
//!
//! # Guides
//!
//! - **[`LinuxVmBuilder`]** - Configuring VMs (CPUs, memory, disks, networking, shared directories)
//! - **[`VmHandle`]** - VM lifecycle management (start, stop, status)
//! - **[`VmConsole`]** - Console automation for integration testing
//! - **[`VmPool`]** - Pre-warmed VM pools for fast acquisition
//! - **[`capabilities`]** - Querying backend/hypervisor support
//!
//! <!-- TODO: Add guide for custom kernel/initrd preparation -->
//! <!-- TODO: Add guide for disk image creation -->
//! <!-- TODO: Add platform-specific notes (macOS vs Linux backends) -->

mod backend;
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

// ============================================================================
// Core API - The types most users need
// ============================================================================

pub use builder::LinuxVmBuilder;
pub use config::Capsa;
pub use console::{ConsoleReader, ConsoleWriter, VmConsole};
pub use handle::{VmHandle, VmStatus};
pub use pool::{PooledVm, VmPool};

// Boot and disk configuration
pub use capsa_core::{DiskImage, ImageFormat, LinuxDirectBootConfig};

// Directory sharing
pub use capsa_core::{MountMode, SharedDir};

// Networking
pub use capsa_core::NetworkMode;

// Errors
pub use capsa_core::{Error, Result};

// ============================================================================
// Advanced API - For specialized use cases
// ============================================================================

// Kernel command line customization
pub use capsa_core::{CmdlineArg, KernelCmdline};

// Fine-grained sharing configuration
pub use capsa_core::{ShareMechanism, Virtio9pConfig, VirtioFsConfig};

// Guest OS type (currently only Linux)
pub use capsa_core::GuestOs;

/// Backend capabilities and hypervisor information.
///
/// This module provides types for querying what features a hypervisor backend
/// supports. Most users won't need these unless dynamically checking platform
/// capabilities.
///
/// # Example
///
/// ```rust,no_run
/// use capsa::capabilities::{BackendCapabilities, available_backends};
///
/// for backend in available_backends() {
///     let caps = backend.capabilities();
///     println!("{}: virtio-fs={}", backend.name(), caps.share_mechanisms.virtio_fs);
/// }
/// ```
pub mod capabilities {
    pub use super::backend::{HypervisorBackend, available_backends};
    pub use capsa_core::{
        BackendCapabilities, BootMethodSupport, GuestOsSupport, HostPlatform, ImageFormatSupport,
        NetworkModeSupport, ShareMechanismSupport,
    };
}
