#[cfg(target_arch = "x86_64")]
mod x86_64;

// TODO: Add aarch64 support
// #[cfg(target_arch = "aarch64")]
// mod aarch64;

#[cfg(target_arch = "x86_64")]
pub use x86_64::*;

// TODO: aarch64 support
// #[cfg(target_arch = "aarch64")]
// pub use aarch64::*;
