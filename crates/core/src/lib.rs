pub mod async_fd;
pub mod backend;
pub mod boot;
pub mod capabilities;
pub mod error;
pub mod macos;
pub mod types;

pub use async_fd::{AsyncOwnedFd, AsyncPipe};
pub use backend::{BackendVmHandle, ConsoleIo, ConsoleStream, HypervisorBackend, VmConfig};
pub use boot::{CmdlineArg, KernelCmdline, LinuxDirectBootConfig};
pub use capabilities::{
    BackendCapabilities, BootMethodSupport, GuestOsSupport, ImageFormatSupport, NetworkModeSupport,
    ShareMechanismSupport,
};
pub use error::{Error, Result};
pub use macos::{DEFAULT_ROOT_DEVICE, macos_cmdline_defaults, macos_virtualization_capabilities};
pub use types::{
    DiskImage, GuestOs, HostPlatform, ImageFormat, MountMode, NetworkMode, ResourceConfig,
    ShareMechanism, SharedDir, Virtio9pConfig, VirtioFsConfig,
};
