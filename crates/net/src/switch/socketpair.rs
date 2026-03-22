use crate::frame::{EthernetFrameIO, FrameReader, FrameWriter};
use crate::util::set_nonblocking;
use nix::sys::socket::{recv, send, setsockopt, socketpair, sockopt, MsgFlags, SockFlag};
use nix::sys::socket::{AddressFamily, SockType};
use std::io;
#[cfg(test)]
use std::os::fd::RawFd;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use tokio::io::unix::AsyncFd;

const ETHERNET_HEADER_SIZE: usize = 14;
const DEFAULT_MTU: usize = 1500;
const MAX_FRAME_SIZE: usize = DEFAULT_MTU + ETHERNET_HEADER_SIZE;

fn errno_to_io_error(e: nix::errno::Errno) -> io::Error {
    io::Error::from_raw_os_error(e as i32)
}

async fn async_recv_frame(fd: &AsyncFd<OwnedFd>, buf: &mut [u8]) -> io::Result<Vec<u8>> {
    loop {
        let mut guard = fd.readable().await?;

        match guard.try_io(|inner| {
            recv(inner.get_ref().as_raw_fd(), buf, MsgFlags::empty()).map_err(errno_to_io_error)
        }) {
            Ok(Ok(n)) => return Ok(buf[..n].to_vec()),
            Ok(Err(e)) => return Err(e),
            Err(_would_block) => continue,
        }
    }
}

/// Frame I/O via Unix socketpair for macOS Virtualization.framework.
///
/// Creates a SOCK_DGRAM socketpair where each message is one ethernet frame.
/// One end is kept by this device for the host network stack, the other
/// is passed to VZFileHandleNetworkDeviceAttachment for the guest.
pub struct SocketPairDevice {
    fd: AsyncFd<OwnedFd>,
    buf: Vec<u8>,
}

/// Read half of a split `SocketPairDevice`.
pub struct SocketPairReader {
    fd: Arc<AsyncFd<OwnedFd>>,
    buf: Vec<u8>,
}

/// Write half of a split `SocketPairDevice`.
pub struct SocketPairWriter {
    fd: Arc<AsyncFd<OwnedFd>>,
}

impl SocketPairDevice {
    /// Create a new socketpair device.
    ///
    /// Returns `(host_device, guest_fd)` where:
    /// - `host_device` implements `EthernetFrameIO` for the userspace network stack
    /// - `guest_fd` should be passed to `VZFileHandleNetworkDeviceAttachment`
    pub fn new() -> io::Result<(Self, OwnedFd)> {
        let (host_fd, guest_fd) = socketpair(
            AddressFamily::Unix,
            SockType::Datagram,
            None,
            SockFlag::empty(),
        )
        .map_err(errno_to_io_error)?;

        set_nonblocking(&host_fd)?;
        increase_socket_buffer(&host_fd)?;
        increase_socket_buffer(&guest_fd)?;

        let fd = AsyncFd::new(host_fd)?;
        let buf = vec![0u8; MAX_FRAME_SIZE];

        Ok((Self { fd, buf }, guest_fd))
    }

    /// Get the raw file descriptor (for debugging/logging).
    #[cfg(test)]
    pub fn as_raw_fd(&self) -> RawFd {
        self.fd.as_raw_fd()
    }
}

impl SocketPairDevice {
    pub fn poll_recv(
        &mut self,
        cx: &mut std::task::Context<'_>,
        buf: &mut [u8],
    ) -> std::task::Poll<io::Result<usize>> {
        loop {
            let mut guard = match self.fd.poll_read_ready(cx) {
                std::task::Poll::Ready(Ok(guard)) => guard,
                std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };

            match guard.try_io(|inner| {
                recv(inner.get_ref().as_raw_fd(), buf, MsgFlags::empty()).map_err(errno_to_io_error)
            }) {
                Ok(result) => return std::task::Poll::Ready(result),
                Err(_would_block) => continue,
            }
        }
    }

    pub fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        let n = send(self.fd.get_ref().as_raw_fd(), frame, MsgFlags::empty())
            .map_err(errno_to_io_error)?;
        if n != frame.len() {
            Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "incomplete frame send",
            ))
        } else {
            Ok(())
        }
    }

    pub async fn send_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        loop {
            let mut guard = self.fd.writable().await?;

            match guard.try_io(|inner| {
                send(inner.get_ref().as_raw_fd(), frame, MsgFlags::empty())
                    .map_err(errno_to_io_error)
            }) {
                Ok(Ok(n)) if n == frame.len() => return Ok(()),
                Ok(Ok(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "incomplete frame send",
                    ));
                }
                // On macOS, ENOBUFS (errno 55) is returned instead of EAGAIN when
                // the socket buffer is full. Unlike EAGAIN, the fd still appears
                // writable to kqueue, so we must yield briefly to avoid a busy loop.
                Ok(Err(e)) if e.raw_os_error() == Some(libc::ENOBUFS) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }

    pub async fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
        async_recv_frame(&self.fd, &mut self.buf).await
    }

    pub fn try_recv_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        match recv(
            self.fd.get_ref().as_raw_fd(),
            &mut self.buf,
            MsgFlags::MSG_DONTWAIT,
        ) {
            Ok(n) => Ok(Some(self.buf[..n].to_vec())),
            Err(nix::errno::Errno::EAGAIN) => Ok(None),
            Err(e) => Err(errno_to_io_error(e)),
        }
    }
}

impl EthernetFrameIO for SocketPairDevice {
    type ReadHalf = SocketPairReader;
    type WriteHalf = SocketPairWriter;

    fn split(self) -> (Self::ReadHalf, Self::WriteHalf) {
        let fd = Arc::new(self.fd);
        (
            SocketPairReader {
                fd: Arc::clone(&fd),
                buf: self.buf,
            },
            SocketPairWriter { fd },
        )
    }
}

impl FrameReader for SocketPairReader {
    async fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
        async_recv_frame(&self.fd, &mut self.buf).await
    }
}

impl FrameWriter for SocketPairWriter {
    async fn send_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        loop {
            let mut guard = self.fd.writable().await?;

            match guard.try_io(|inner| {
                send(inner.get_ref().as_raw_fd(), frame, MsgFlags::empty())
                    .map_err(errno_to_io_error)
            }) {
                Ok(Ok(n)) if n == frame.len() => return Ok(()),
                Ok(Ok(_)) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "incomplete frame send",
                    ));
                }
                // On macOS, ENOBUFS (errno 55) is returned instead of EAGAIN when
                // the socket buffer is full. Unlike EAGAIN, the fd still appears
                // writable to kqueue, so we must yield briefly to avoid a busy loop.
                Ok(Err(e)) if e.raw_os_error() == Some(libc::ENOBUFS) => {
                    tokio::task::yield_now().await;
                    continue;
                }
                Ok(Err(e)) => return Err(e),
                Err(_would_block) => continue,
            }
        }
    }
}

const SOCKET_BUFFER_SIZE: usize = 256 * 1024;

pub(crate) fn increase_socket_buffer(fd: &OwnedFd) -> io::Result<()> {
    setsockopt(&fd, sockopt::SndBuf, &SOCKET_BUFFER_SIZE).map_err(errno_to_io_error)?;
    setsockopt(&fd, sockopt::RcvBuf, &SOCKET_BUFFER_SIZE).map_err(errno_to_io_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn socketpair_creation_returns_valid_fds() {
        let (device, guest_fd) = SocketPairDevice::new().expect("Failed to create socketpair");
        assert!(device.as_raw_fd() >= 0);
        assert!(guest_fd.as_raw_fd() >= 0);
        assert_ne!(device.as_raw_fd(), guest_fd.as_raw_fd());
    }

    #[tokio::test]
    async fn socketpair_sets_send_and_recv_buffers_on_both_fds() {
        use nix::sys::socket::getsockopt;

        let (device, guest_fd) = SocketPairDevice::new().expect("Failed to create socketpair");
        let expected_min = 256 * 1024;

        let host_sndbuf: usize =
            getsockopt(&device.fd.get_ref(), sockopt::SndBuf).expect("getsockopt SndBuf");
        let host_rcvbuf: usize =
            getsockopt(&device.fd.get_ref(), sockopt::RcvBuf).expect("getsockopt RcvBuf");
        let guest_sndbuf: usize =
            getsockopt(&guest_fd, sockopt::SndBuf).expect("getsockopt SndBuf");
        let guest_rcvbuf: usize =
            getsockopt(&guest_fd, sockopt::RcvBuf).expect("getsockopt RcvBuf");

        assert!(
            host_sndbuf >= expected_min,
            "host SndBuf {host_sndbuf} < {expected_min}"
        );
        assert!(
            host_rcvbuf >= expected_min,
            "host RcvBuf {host_rcvbuf} < {expected_min}"
        );
        assert!(
            guest_sndbuf >= expected_min,
            "guest SndBuf {guest_sndbuf} < {expected_min}"
        );
        assert!(
            guest_rcvbuf >= expected_min,
            "guest RcvBuf {guest_rcvbuf} < {expected_min}"
        );
    }

    #[tokio::test]
    async fn send_and_receive_frame_via_socketpair() {
        let (mut host_device, guest_fd) =
            SocketPairDevice::new().expect("Failed to create socketpair");

        // Send a frame from host to guest
        let test_frame = b"test ethernet frame";
        host_device.send(test_frame).expect("Failed to send frame");

        // Receive on the guest side
        let mut buf = [0u8; 100];
        let n = recv(guest_fd.as_raw_fd(), &mut buf, MsgFlags::empty())
            .expect("Failed to receive frame");
        assert!(n > 0);
        assert_eq!(&buf[..n], test_frame);
    }

    #[tokio::test]
    async fn receive_frame_from_guest_side() {
        let (host_device, guest_fd) = SocketPairDevice::new().expect("Failed to create socketpair");

        // Send from guest side
        let test_frame = b"guest frame data";
        let n = send(guest_fd.as_raw_fd(), test_frame, MsgFlags::empty())
            .expect("Failed to send frame");
        assert_eq!(n, test_frame.len());

        // Receive on host side using poll_recv with proper async waiting
        let mut buf = [0u8; 100];
        let mut host_device = host_device;

        let len = std::future::poll_fn(|cx| host_device.poll_recv(cx, &mut buf))
            .await
            .expect("Failed to receive frame");
        assert_eq!(&buf[..len], test_frame);
    }

    #[tokio::test]
    async fn recv_frame_waits_and_returns_frame() {
        let (mut host_device, guest_fd) =
            SocketPairDevice::new().expect("Failed to create socketpair");

        let test_frame = b"async frame data";
        send(guest_fd.as_raw_fd(), test_frame, MsgFlags::empty()).expect("Failed to send frame");

        let frame = host_device
            .recv_frame()
            .await
            .expect("Failed to recv_frame");
        assert_eq!(&frame, test_frame);
    }

    #[tokio::test]
    async fn try_recv_frame_returns_none_when_empty() {
        let (mut host_device, _guest_fd) =
            SocketPairDevice::new().expect("Failed to create socketpair");

        let result = host_device.try_recv_frame().expect("try_recv_frame failed");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn try_recv_frame_returns_available_frame() {
        let (mut host_device, guest_fd) =
            SocketPairDevice::new().expect("Failed to create socketpair");

        let test_frame = b"non-blocking frame";
        send(guest_fd.as_raw_fd(), test_frame, MsgFlags::empty()).expect("Failed to send frame");

        let frame = host_device
            .try_recv_frame()
            .expect("try_recv_frame failed")
            .expect("expected Some(frame)");
        assert_eq!(&frame, test_frame);
    }

    #[tokio::test]
    async fn recv_buffer_reused_across_varying_frame_sizes() {
        let (mut host_device, guest_fd) =
            SocketPairDevice::new().expect("Failed to create socketpair");

        let small_frame = vec![0xAA; 100];
        let large_frame = vec![0xBB; 1500];

        send(guest_fd.as_raw_fd(), &small_frame, MsgFlags::empty()).unwrap();
        send(guest_fd.as_raw_fd(), &large_frame, MsgFlags::empty()).unwrap();

        let mut result1 = host_device.recv_frame().await.unwrap();
        assert_eq!(result1.len(), 100);
        assert!(result1.iter().all(|&b| b == 0xAA));

        result1[0] = 0xFF;

        let result2 = host_device.recv_frame().await.unwrap();
        assert_eq!(result2.len(), 1500);
        assert!(result2.iter().all(|&b| b == 0xBB));

        assert_eq!(result1[0], 0xFF);
    }

    #[tokio::test]
    async fn send_frame_handles_backpressure() {
        use nix::sys::socket::getsockopt;

        // Use a small send buffer so it fills quickly with 1500-byte frames.
        let (host_fd, guest_fd) = socketpair(
            AddressFamily::Unix,
            SockType::Datagram,
            None,
            SockFlag::empty(),
        )
        .expect("socketpair failed");

        set_nonblocking(&host_fd).expect("set_nonblocking failed");
        let requested_buf: usize = 8 * 1024;
        setsockopt(&host_fd, sockopt::SndBuf, &requested_buf).expect("setsockopt failed");

        // Verify the actual buffer size (kernel may clamp to a minimum).
        let actual_buf: usize = getsockopt(&host_fd, sockopt::SndBuf).expect("getsockopt failed");
        let frame_size = 1500usize;
        // Calculate how many frames fit in the buffer (kernel often doubles the value).
        let buffer_capacity_frames = actual_buf / frame_size;

        let fd = AsyncFd::new(host_fd).expect("AsyncFd failed");
        let mut host_device = SocketPairDevice {
            fd,
            buf: vec![0u8; MAX_FRAME_SIZE],
        };

        let frame = vec![0u8; frame_size];
        // Send enough frames to guarantee backpressure: 3x buffer capacity.
        let frame_count = (buffer_capacity_frames * 3).max(30);

        // Spawn a drain task that starts after a delay, forcing send_frame() to wait.
        let drain_start_delay = std::time::Duration::from_millis(50);
        let drain_handle = tokio::spawn(async move {
            tokio::time::sleep(drain_start_delay).await;

            let mut buf = vec![0u8; 2000];
            let mut received = 0;
            while received < frame_count {
                match recv(guest_fd.as_raw_fd(), &mut buf, MsgFlags::MSG_DONTWAIT) {
                    Ok(_) => received += 1,
                    Err(nix::errno::Errno::EAGAIN) => {
                        tokio::time::sleep(std::time::Duration::from_millis(1)).await;
                    }
                    Err(e) => panic!("recv error: {}", e),
                }
            }
            received
        });

        // Measure time to verify we actually waited for backpressure.
        let start = std::time::Instant::now();
        let send_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for _ in 0..frame_count {
                host_device
                    .send_frame(&frame)
                    .await
                    .expect("send_frame should handle backpressure");
            }
        })
        .await;
        let elapsed = start.elapsed();

        assert!(send_result.is_ok(), "send_frame timed out - possible hang");

        // Verify we waited at least as long as the drain delay, proving backpressure.
        assert!(
            elapsed >= drain_start_delay,
            "send_frame completed in {:?}, expected to wait at least {:?} for drain. \
             Buffer size: {} bytes (~{} frames). This suggests backpressure wasn't exercised.",
            elapsed,
            drain_start_delay,
            actual_buf,
            buffer_capacity_frames
        );

        let received = tokio::time::timeout(std::time::Duration::from_secs(5), drain_handle)
            .await
            .expect("drain task timed out")
            .expect("drain task panicked");

        assert_eq!(received, frame_count);
    }
}
