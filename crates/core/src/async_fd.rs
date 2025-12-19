use std::os::fd::{AsRawFd, OwnedFd};
use std::pin::Pin;
use std::task::{Context, Poll};

use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};

fn poll_read_fd(
    fd: &AsyncFd<OwnedFd>,
    cx: &mut Context<'_>,
    buf: &mut ReadBuf<'_>,
) -> Poll<std::io::Result<()>> {
    loop {
        let mut guard = match fd.poll_read_ready(cx) {
            Poll::Ready(Ok(guard)) => guard,
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        };

        let raw_fd = fd.get_ref().as_raw_fd();
        let unfilled = buf.initialize_unfilled();

        match nix::unistd::read(raw_fd, unfilled) {
            Ok(n) => {
                buf.advance(n);
                return Poll::Ready(Ok(()));
            }
            Err(nix::errno::Errno::EAGAIN) => {
                guard.clear_ready();
                continue;
            }
            Err(e) => {
                return Poll::Ready(Err(std::io::Error::other(e)));
            }
        }
    }
}

fn poll_write_fd(
    fd: &AsyncFd<OwnedFd>,
    cx: &mut Context<'_>,
    buf: &[u8],
) -> Poll<std::io::Result<usize>> {
    loop {
        let mut guard = match fd.poll_write_ready(cx) {
            Poll::Ready(Ok(guard)) => guard,
            Poll::Ready(Err(e)) => return Poll::Ready(Err(e)),
            Poll::Pending => return Poll::Pending,
        };

        match nix::unistd::write(fd.get_ref(), buf) {
            Ok(n) => return Poll::Ready(Ok(n)),
            Err(nix::errno::Errno::EAGAIN) => {
                guard.clear_ready();
                continue;
            }
            Err(e) => {
                return Poll::Ready(Err(std::io::Error::other(e)));
            }
        }
    }
}

pub struct AsyncOwnedFd(AsyncFd<OwnedFd>);

impl AsyncOwnedFd {
    pub fn new(fd: OwnedFd) -> std::io::Result<Self> {
        Ok(Self(AsyncFd::new(fd)?))
    }
}

impl AsyncRead for AsyncOwnedFd {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        poll_read_fd(&self.0, cx, buf)
    }
}

impl AsyncWrite for AsyncOwnedFd {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        poll_write_fd(&self.0, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

pub struct AsyncPipe {
    read_fd: AsyncFd<OwnedFd>,
    write_fd: AsyncFd<OwnedFd>,
}

impl AsyncPipe {
    pub fn new(read_fd: OwnedFd, write_fd: OwnedFd) -> std::io::Result<Self> {
        Ok(Self {
            read_fd: AsyncFd::new(read_fd)?,
            write_fd: AsyncFd::new(write_fd)?,
        })
    }
}

impl AsyncRead for AsyncPipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        poll_read_fd(&self.read_fd, cx, buf)
    }
}

impl AsyncWrite for AsyncPipe {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        poll_write_fd(&self.write_fd, cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }

    fn poll_shutdown(self: Pin<&mut Self>, _cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn async_owned_fd_can_create_from_pipe() {
        let (read_fd, _write_fd) = nix::unistd::pipe().unwrap();
        let async_fd = AsyncOwnedFd::new(read_fd);
        assert!(async_fd.is_ok());
    }

    #[tokio::test]
    async fn async_pipe_can_create_from_pipe_pair() {
        let (read_fd, write_fd) = nix::unistd::pipe().unwrap();
        let async_pipe = AsyncPipe::new(read_fd, write_fd);
        assert!(async_pipe.is_ok());
    }
}
