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
//! use capsa::{Capsa, LinuxDirectBootConfig};
//!
//! # async fn example() -> capsa::Result<()> {
//! let config = LinuxDirectBootConfig::new("./kernel", "./initrd")
//!     .with_root_disk("./rootfs.raw");
//!
//! let vm = Capsa::vm(config)
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
//! - **[Getting Started](guides::getting_started)** - Creating your first VM
//! - **[Console Automation](guides::console_automation)** - Testing patterns with the console
//! - **[VM Pools](guides::vm_pools)** - Pre-warmed VMs for faster startup
//! - **[Shared Directories](guides::shared_directories)** - File sharing between host and guest
//!
//! For API reference, see the individual type documentation:
//! [`LinuxVmBuilder`], [`VmHandle`], [`VmConsole`], [`VmPool`].

mod backend;
mod builder;
mod cluster;
mod config;
mod console;
pub mod guides;
mod handle;
mod pool;
pub mod sandbox;
mod vsock;

// TODO: stop exporting test-utils externally as all it does is expose VMs from test-vms.nix
// there probably is some useful test utilities for capsa that could be helpful
// for users of the library, but these are probably not it (unless things like
// the minimal VMs are bundled with the library)
#[cfg(feature = "test-utils")]
pub mod test_utils;

// ============================================================================
// Core API - The types most users need
// ============================================================================

pub use builder::{LinuxVmBuilder, UefiVmBuilder};
pub use config::{BootConfig, Capsa};
pub use console::{ConsoleReader, ConsoleWriter, VmConsole};
pub use handle::{VmHandle, VmStatus};
pub use pool::{PooledVm, VmPool};
pub use sandbox::{CapsaSandboxConfig, HasMainProcess, NoMainProcess, SandboxBuilder};

// Boot and disk configuration
pub use capsa_core::{
    DiskImage, EfiVariableStore, ImageFormat, LinuxDirectBootConfig, UefiBootConfig,
};

// Directory sharing
pub use capsa_core::{MountMode, SharedDir};

// Networking
pub use capsa_core::{NetworkClusterConfig, NetworkMode};
pub use cluster::NetworkCluster;

// Vsock (VM sockets for host-guest communication)
pub use capsa_core::{VsockConfig, VsockPortConfig};
pub use vsock::VsockSocket;

// Errors
pub use capsa_core::{Error, Result};

// ============================================================================
// Advanced API - For specialized use cases
// ============================================================================

// Kernel command line customization
pub use capsa_core::KernelCmdline;

// Fine-grained sharing configuration
pub use capsa_core::ShareMechanism;

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
