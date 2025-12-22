use capsa_core::{AsyncOwnedFd, ConsoleStream};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

pub fn try_get_fd3() -> Option<OwnedFd> {
    use nix::fcntl::{FcntlArg, fcntl};

    let fd = 3;
    match fcntl(fd, FcntlArg::F_GETFD) {
        Ok(_) => Some(unsafe { OwnedFd::from_raw_fd(fd) }),
        Err(_) => None,
    }
}

pub fn spawn_proxy(fd3: OwnedFd, console: ConsoleStream) {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};

    let flags = fcntl(fd3.as_raw_fd(), FcntlArg::F_GETFL).unwrap_or(0);
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    let _ = fcntl(fd3.as_raw_fd(), FcntlArg::F_SETFL(flags));

    tokio::spawn(async move {
        let async_fd3 = match AsyncOwnedFd::new(fd3) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::error!("Failed to create async fd3: {}", e);
                return;
            }
        };

        let (mut console_read, mut console_write) = tokio::io::split(console);
        let (mut fd3_read, mut fd3_write) = tokio::io::split(async_fd3);

        let console_to_fd3 = async {
            let mut buf = [0u8; 4096];
            loop {
                match console_read.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if fd3_write.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };

        let fd3_to_console = async {
            let mut buf = [0u8; 4096];
            loop {
                match fd3_read.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if console_write.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };

        tokio::select! {
            _ = console_to_fd3 => {}
            _ = fd3_to_console => {}
        }
    });
}
