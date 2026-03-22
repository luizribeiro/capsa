use std::future::Future;
use std::os::fd::AsRawFd;

use nix::fcntl::{fcntl, FcntlArg, OFlag};

pub(crate) fn set_nonblocking(fd: &impl AsRawFd) -> std::io::Result<()> {
    let flags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)
        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
    let flags = OFlag::from_bits_truncate(flags);
    let new_flags = flags | OFlag::O_NONBLOCK;
    fcntl(fd.as_raw_fd(), FcntlArg::F_SETFL(new_flags))
        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))?;
    Ok(())
}

pub(crate) fn spawn_named<F>(name: &str, future: F) -> tokio::task::JoinHandle<F::Output>
where
    F: Future + Send + 'static,
    F::Output: Send + 'static,
{
    #[cfg(tokio_unstable)]
    {
        tokio::task::Builder::new()
            .name(name)
            .spawn(future)
            .expect("failed to spawn task")
    }

    #[cfg(not(tokio_unstable))]
    {
        let _ = name;
        tokio::spawn(future)
    }
}
