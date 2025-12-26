//! Native Virtualization.framework backend for macOS.
//!
//! This crate provides direct bindings to Apple's Virtualization.framework.
//! It is used by the `capsa-apple-vzd` daemon to run VMs.

mod backend;
mod delegate;
mod handle;
mod vm;
mod vsock;

pub use backend::NativeVirtualizationBackend;
