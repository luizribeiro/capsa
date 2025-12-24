mod error;
mod frame_io;

#[cfg(target_os = "macos")]
mod socketpair;

#[cfg(target_os = "linux")]
mod tap;

pub use error::NetError;
pub use frame_io::FrameIO;

#[cfg(target_os = "macos")]
pub use socketpair::SocketPairDevice;

#[cfg(target_os = "linux")]
pub use tap::TapDevice;
