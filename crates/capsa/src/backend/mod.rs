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
    BackendCapabilities, BackendVmHandle, ConsoleIo, ConsoleStream, HostPlatform,
    HypervisorBackend, KernelCmdline, Result, VmConfig,
};

/// Returns all compiled-in backends.
pub fn available_backends() -> Vec<Box<dyn HypervisorBackend>> {
    let mut backends: Vec<Box<dyn HypervisorBackend>> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        #[cfg(feature = "macos-subprocess")]
        backends.push(Box::new(MacOsBackend::subprocess()));

        #[cfg(feature = "macos-native")]
        backends.push(Box::new(MacOsBackend::native()));

        #[cfg(feature = "vfkit")]
        backends.push(Box::new(MacOsBackend::vfkit()));
    }

    backends
}

pub(crate) fn select_backend() -> Result<Box<dyn HypervisorBackend>> {
    for backend in available_backends() {
        if backend.is_available() {
            return Ok(backend);
        }
    }
    Err(capsa_core::Error::NoBackendAvailable)
}
