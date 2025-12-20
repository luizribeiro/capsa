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

/// Information about a hypervisor backend.
#[derive(Debug, Clone)]
pub struct BackendInfo {
    /// Backend name (e.g., "vfkit", "native-virtualization")
    pub name: &'static str,
    /// Host platform the backend runs on
    pub platform: HostPlatform,
    /// Whether the backend is available on this system
    pub available: bool,
    /// Backend capabilities
    pub capabilities: BackendCapabilities,
}

/// Returns information about all compiled-in backends.
pub fn available_backends() -> Vec<BackendInfo> {
    available_backends_with_constructors()
        .into_iter()
        .map(|(info, _)| info)
        .collect()
}

pub(crate) fn select_backend() -> Result<Box<dyn HypervisorBackend>> {
    // Find the first available backend (priority order is determined by available_backends_with_constructors)
    for (info, constructor) in available_backends_with_constructors() {
        if info.available {
            return Ok(constructor());
        }
    }
    Err(capsa_core::Error::NoBackendAvailable)
}

type BackendConstructor = fn() -> Box<dyn HypervisorBackend>;

fn available_backends_with_constructors() -> Vec<(BackendInfo, BackendConstructor)> {
    let mut backends: Vec<(BackendInfo, BackendConstructor)> = Vec::new();

    #[cfg(target_os = "macos")]
    {
        #[cfg(feature = "macos-subprocess")]
        {
            let backend = MacOsBackend::subprocess();
            backends.push((
                BackendInfo {
                    name: backend.name(),
                    platform: backend.platform(),
                    available: backend.is_available(),
                    capabilities: backend.capabilities().clone(),
                },
                || Box::new(MacOsBackend::subprocess()),
            ));
        }

        #[cfg(feature = "macos-native")]
        {
            let backend = MacOsBackend::native();
            backends.push((
                BackendInfo {
                    name: backend.name(),
                    platform: backend.platform(),
                    available: backend.is_available(),
                    capabilities: backend.capabilities().clone(),
                },
                || Box::new(MacOsBackend::native()),
            ));
        }

        #[cfg(feature = "vfkit")]
        {
            let backend = MacOsBackend::vfkit();
            backends.push((
                BackendInfo {
                    name: backend.name(),
                    platform: backend.platform(),
                    available: backend.is_available(),
                    capabilities: backend.capabilities().clone(),
                },
                || Box::new(MacOsBackend::vfkit()),
            ));
        }
    }

    backends
}
