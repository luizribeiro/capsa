//! FUSE protocol implementation for virtio-fs.
//!
//! This module provides the FUSE protocol handling for the virtio-fs device.
//! It includes protocol types, inode management, and file handle tracking.

mod handle;
mod inode;
mod protocol;

pub use handle::HandleTable;
pub use inode::{InodeTable, errno_from_io, metadata_to_attr};
pub use protocol::*;
