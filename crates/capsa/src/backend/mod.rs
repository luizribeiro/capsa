//! Backend implementations for different hypervisors.
//!
//! This module provides the hypervisor backend abstraction and platform-specific
//! implementations. Users typically don't need to interact with this module directly;
//! the [`Capsa`](crate::Capsa) builder automatically selects an available backend.

#[cfg(all(target_os = "macos", feature = "macos-subprocess"))]
mod macos;

#[cfg(target_os = "linux")]
mod linux;

#[cfg(all(target_os = "macos", feature = "macos-subprocess"))]
pub use macos::MacOsBackend;

#[cfg(all(target_os = "linux", feature = "linux-kvm"))]
pub use linux::LinuxKvmBackend;

pub use capsa_core::{HypervisorBackend, Result};

/// Returns all compiled-in backends.
#[allow(clippy::vec_init_then_push, unused_mut)]
pub fn available_backends() -> Vec<Box<dyn HypervisorBackend>> {
    let mut backends: Vec<Box<dyn HypervisorBackend>> = Vec::new();

    #[cfg(all(target_os = "macos", feature = "macos-subprocess"))]
    backends.push(Box::new(MacOsBackend::new()));

    #[cfg(all(target_os = "linux", feature = "linux-kvm"))]
    backends.push(Box::new(LinuxKvmBackend::new()));

    backends
}

pub(crate) fn select_backend() -> Result<Box<dyn HypervisorBackend>> {
    for backend in available_backends() {
        if backend.is_available() {
            return Ok(backend);
        }
    }
    Err(capsa_core::Error::NoBackendAvailable)
}
