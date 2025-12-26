use capsa_core::AsyncOwnedFd;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::pty::{OpenptyResult, openpty};
use nix::sys::termios::{self, ControlFlags, InputFlags, LocalFlags, OutputFlags, SetArg};
use std::os::fd::{AsFd, AsRawFd, OwnedFd};

pub struct Pty {
    pub master: OwnedFd,
    pub slave: OwnedFd,
}

impl Pty {
    pub fn new() -> std::io::Result<Self> {
        let OpenptyResult { master, slave } = openpty(None, None).map_err(std::io::Error::other)?;

        let slave_fd = slave.as_fd();
        if let Ok(mut termios) = termios::tcgetattr(slave_fd) {
            // Disable input processing that would intercept control characters
            termios.input_flags.remove(InputFlags::IGNBRK);
            termios.input_flags.remove(InputFlags::BRKINT);
            termios.input_flags.remove(InputFlags::PARMRK);
            termios.input_flags.remove(InputFlags::ISTRIP);
            termios.input_flags.remove(InputFlags::INLCR);
            termios.input_flags.remove(InputFlags::IGNCR);
            termios.input_flags.remove(InputFlags::ICRNL);
            termios.input_flags.remove(InputFlags::IXON);

            // Keep output processing for proper line endings (\n -> \r\n)
            termios.output_flags.insert(OutputFlags::OPOST);
            termios.output_flags.insert(OutputFlags::ONLCR);

            // Disable local flags that would intercept signals or do line editing
            termios.local_flags.remove(LocalFlags::ECHO);
            termios.local_flags.remove(LocalFlags::ECHONL);
            termios.local_flags.remove(LocalFlags::ICANON);
            termios.local_flags.remove(LocalFlags::ISIG);
            termios.local_flags.remove(LocalFlags::IEXTEN);

            // 8-bit clean
            termios.control_flags.remove(ControlFlags::CSIZE);
            termios.control_flags.remove(ControlFlags::PARENB);
            termios.control_flags.insert(ControlFlags::CS8);

            let _ = termios::tcsetattr(slave_fd, SetArg::TCSANOW, &termios);
        }

        Ok(Self { master, slave })
    }

    pub fn into_async_master(self) -> std::io::Result<AsyncOwnedFd> {
        let flags =
            fcntl(self.master.as_raw_fd(), FcntlArg::F_GETFL).map_err(std::io::Error::other)?;
        let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
        fcntl(self.master.as_raw_fd(), FcntlArg::F_SETFL(flags)).map_err(std::io::Error::other)?;

        AsyncOwnedFd::new(self.master)
    }
}
