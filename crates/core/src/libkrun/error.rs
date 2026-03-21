use anyhow::{bail, Result};

pub(super) fn check_rc(rc: i32, context: &str) -> Result<()> {
    if rc < 0 {
        bail!("{context}: {}", os_error_from_neg_errno(rc));
    }
    Ok(())
}

pub(super) fn os_error_from_neg_errno(rc: i32) -> std::io::Error {
    std::io::Error::from_raw_os_error(-rc)
}
