//! Network file descriptor handling for cluster mode.

use std::os::fd::{FromRawFd, OwnedFd, RawFd};

const NETWORK_FD: RawFd = 4;

/// Try to get fd 4 (network guest fd) if it was passed from parent.
pub fn try_get_network_fd() -> Option<OwnedFd> {
    use nix::fcntl::{FcntlArg, fcntl};

    match fcntl(NETWORK_FD, FcntlArg::F_GETFD) {
        Ok(_) => Some(unsafe { OwnedFd::from_raw_fd(NETWORK_FD) }),
        Err(_) => None,
    }
}
