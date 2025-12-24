use std::io;
use std::task::{Context, Poll};

/// Abstraction for ethernet frame transport.
///
/// This trait allows the network stack to work with different frame
/// sources: socketpairs on macOS, TAP devices on Linux, or virtual
/// switch ports for multi-VM networking.
pub trait FrameIO: Send + 'static {
    /// Maximum transmission unit (typically 1500 for ethernet).
    fn mtu(&self) -> usize {
        1500
    }

    /// Poll for an incoming ethernet frame.
    ///
    /// Returns the number of bytes read into `buf` when a frame is available.
    fn poll_recv(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>>;

    /// Send an ethernet frame.
    fn send(&mut self, frame: &[u8]) -> io::Result<()>;
}
