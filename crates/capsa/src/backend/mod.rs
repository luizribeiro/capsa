#[cfg(all(target_os = "macos", feature = "macos-subprocess"))]
mod subprocess;
#[cfg(all(target_os = "macos", feature = "vfkit"))]
mod vfkit;

#[cfg(all(target_os = "macos", feature = "macos-native"))]
pub use capsa_backend_native::NativeVirtualizationBackend;
#[cfg(all(target_os = "macos", feature = "macos-subprocess"))]
pub use subprocess::SubprocessVirtualizationBackend;
#[cfg(all(target_os = "macos", feature = "vfkit"))]
pub use vfkit::VfkitBackend;

pub use capsa_core::{
    BackendCapabilities, BackendVmHandle, ConsoleIo, ConsoleStream, HypervisorBackend,
    InternalVmConfig, KernelCmdline, Result,
};

pub(crate) fn select_backend() -> Result<Box<dyn HypervisorBackend>> {
    #[cfg(target_os = "macos")]
    {
        #[cfg(feature = "macos-subprocess")]
        {
            let subprocess = SubprocessVirtualizationBackend::new();
            if subprocess.is_available() {
                return Ok(Box::new(subprocess));
            }
        }

        #[cfg(feature = "macos-native")]
        {
            let native = NativeVirtualizationBackend::new();
            if native.is_available() {
                return Ok(Box::new(native));
            }
        }

        #[cfg(feature = "vfkit")]
        {
            let vfkit = VfkitBackend::new();
            if vfkit.is_available() {
                return Ok(Box::new(vfkit));
            }
        }
    }

    Err(capsa_core::Error::NoBackendAvailable)
}
