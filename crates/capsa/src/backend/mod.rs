#[cfg(target_os = "macos")]
mod macos;

#[cfg(all(
    target_os = "macos",
    any(
        feature = "vfkit",
        feature = "macos-subprocess",
        feature = "macos-native"
    )
))]
pub use macos::MacOsBackend;

pub use capsa_core::{
    BackendCapabilities, BackendVmHandle, ConsoleIo, ConsoleStream, HypervisorBackend,
    InternalVmConfig, KernelCmdline, Result,
};

pub(crate) fn select_backend() -> Result<Box<dyn HypervisorBackend>> {
    #[cfg(target_os = "macos")]
    {
        #[cfg(feature = "macos-subprocess")]
        {
            let backend = MacOsBackend::subprocess();
            if backend.is_available() {
                return Ok(Box::new(backend));
            }
        }

        #[cfg(feature = "macos-native")]
        {
            let backend = MacOsBackend::native();
            if backend.is_available() {
                return Ok(Box::new(backend));
            }
        }

        #[cfg(feature = "vfkit")]
        {
            let backend = MacOsBackend::vfkit();
            if backend.is_available() {
                return Ok(Box::new(backend));
            }
        }
    }

    Err(capsa_core::Error::NoBackendAvailable)
}
