//! Native Virtualization.framework backend for macOS.
//!
//! This backend uses Apple's Virtualization.framework directly instead of
//! spawning vfkit as a subprocess, eliminating process spawn overhead.
//!
//! This crate only compiles on macOS. On other platforms, it provides no exports.

// TODO: make sure all capabilities are covered by tests using the minimal VM

#[cfg(target_os = "macos")]
mod delegate;
#[cfg(target_os = "macos")]
mod handle;
#[cfg(target_os = "macos")]
mod vm;
#[cfg(target_os = "macos")]
mod vsock;

#[cfg(target_os = "macos")]
mod backend;

#[cfg(target_os = "macos")]
pub use backend::NativeVirtualizationBackend;
