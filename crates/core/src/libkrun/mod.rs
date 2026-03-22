mod error;
mod ffi;
mod vm;

pub(crate) use vm::{init_logging, Configured, KrunVm};

pub(crate) const fn compat_net_features() -> u32 {
    ffi::COMPAT_NET_FEATURES
}
