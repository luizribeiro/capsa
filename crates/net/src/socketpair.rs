use crate::FrameIO;
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd, RawFd};
use std::task::{Context, Poll};
use tokio::io::unix::AsyncFd;

/// Frame I/O via Unix socketpair for macOS Virtualization.framework.
///
/// Creates a SOCK_DGRAM socketpair where each message is one ethernet frame.
/// One end is kept by this device for the host network stack, the other
/// is passed to VZFileHandleNetworkDeviceAttachment for the guest.
pub struct SocketPairDevice {
    fd: AsyncFd<OwnedFd>,
}

impl SocketPairDevice {
    /// Create a new socketpair device.
    ///
    /// Returns `(host_device, guest_fd)` where:
    /// - `host_device` implements `FrameIO` for the userspace network stack
    /// - `guest_fd` should be passed to `VZFileHandleNetworkDeviceAttachment`
    pub fn new() -> io::Result<(Self, OwnedFd)> {
        let mut fds: [RawFd; 2] = [-1, -1];

        let result =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) };

        if result < 0 {
            return Err(io::Error::last_os_error());
        }

        let host_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let guest_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        set_nonblocking(&host_fd)?;

        let fd = AsyncFd::new(host_fd)?;

        Ok((Self { fd }, guest_fd))
    }

    /// Get the raw file descriptor (for debugging/logging).
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl FrameIO for SocketPairDevice {
    fn poll_recv(&mut self, cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        loop {
            let mut guard = match self.fd.poll_read_ready(cx) {
                Poll::Ready(Ok(guard)) => guard,
                Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
                Poll::Pending => return Poll::Pending,
            };

            match guard.try_io(|inner| {
                let fd = inner.as_raw_fd();
                let n = unsafe { libc::recv(fd, buf.as_mut_ptr() as *mut _, buf.len(), 0) };
                if n < 0 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(n as usize)
                }
            }) {
                Ok(result) => return Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        let fd = self.fd.as_raw_fd();
        let n = unsafe { libc::send(fd, frame.as_ptr() as *const _, frame.len(), 0) };
        if n < 0 {
            Err(io::Error::last_os_error())
        } else if n as usize != frame.len() {
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "incomplete frame send",
            ))
        } else {
            Ok(())
        }
    }
}

fn set_nonblocking(fd: &OwnedFd) -> io::Result<()> {
    let raw = fd.as_raw_fd();
    let flags = unsafe { libc::fcntl(raw, libc::F_GETFL) };
    if flags < 0 {
        return Err(io::Error::last_os_error());
    }
    let result = unsafe { libc::fcntl(raw, libc::F_SETFL, flags | libc::O_NONBLOCK) };
    if result < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
