//! Native Virtualization.framework backend for macOS.
//!
//! This backend uses Apple's Virtualization.framework directly instead of
//! spawning vfkit as a subprocess, eliminating process spawn overhead.

mod backend;
mod delegate;
mod handle;
mod vm;
mod vsock;

pub use backend::NativeVirtualizationBackend;
