use crate::FrameIO;
use std::io;
use std::os::fd::OwnedFd;
use std::task::{Context, Poll};

/// TAP device for Linux KVM networking.
///
/// This will be fully implemented in Phase 2.
pub struct TapDevice {
    _fd: OwnedFd,
}

impl TapDevice {
    /// Create a new TAP device with the given name pattern.
    ///
    /// The name can include `%d` which will be replaced with a number
    /// to create a unique interface name (e.g., "capsa%d" -> "capsa0").
    pub fn new(_name: &str) -> io::Result<Self> {
        // Phase 2: Implement TAP device creation
        // - Open /dev/net/tun
        // - ioctl TUNSETIFF with IFF_TAP | IFF_NO_PI
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "TAP device not yet implemented (Phase 2)",
        ))
    }

    /// Create from an existing TAP file descriptor.
    pub fn from_fd(_fd: OwnedFd) -> io::Result<Self> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "TAP device not yet implemented (Phase 2)",
        ))
    }
}

impl FrameIO for TapDevice {
    fn poll_recv(&mut self, _cx: &mut Context<'_>, _buf: &mut [u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "TAP device not yet implemented (Phase 2)",
        )))
    }

    fn send(&mut self, _frame: &[u8]) -> io::Result<()> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "TAP device not yet implemented (Phase 2)",
        ))
    }
}
