use crate::types::GuestOs;

/// Errors that can occur when using Capsa.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no suitable hypervisor backend available")]
    NoBackendAvailable,

    #[error("backend '{name}' is not available: {reason}")]
    BackendUnavailable { name: String, reason: String },

    #[error("feature not supported: {0}")]
    UnsupportedFeature(String),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("missing required configuration: {0}")]
    MissingConfig(String),

    #[error("guest OS not supported by backend: {0:?}")]
    UnsupportedGuestOs(GuestOs),

    #[error("VM failed to start: {0}")]
    StartFailed(String),

    #[error("VM is not running")]
    NotRunning,

    #[error("VM is already running")]
    AlreadyRunning,

    #[error("console not enabled for this VM")]
    ConsoleNotEnabled,

    #[error("operation timed out: {0}")]
    Timeout(String),

    #[error("pattern not found in console output: {pattern}")]
    PatternNotFound { pattern: String },

    #[error("agent error: {0}")]
    Agent(String),

    #[error("no VMs available in pool")]
    PoolEmpty,

    #[error("pool is shutting down")]
    PoolShutdown,

    #[error("hypervisor error: {0}")]
    Hypervisor(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_no_backend() {
        let err = Error::NoBackendAvailable;
        assert_eq!(err.to_string(), "no suitable hypervisor backend available");
    }

    #[test]
    fn error_display_backend_unavailable() {
        let err = Error::BackendUnavailable {
            name: "kvm".to_string(),
            reason: "not available".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "backend 'kvm' is not available: not available"
        );
    }

    #[test]
    fn error_display_unsupported_feature() {
        let err = Error::UnsupportedFeature("virtio-gpu".to_string());
        assert_eq!(err.to_string(), "feature not supported: virtio-gpu");
    }

    #[test]
    fn error_display_invalid_config() {
        let err = Error::InvalidConfig("invalid CPU count".to_string());
        assert_eq!(err.to_string(), "invalid configuration: invalid CPU count");
    }

    #[test]
    fn error_display_unsupported_guest_os() {
        let err = Error::UnsupportedGuestOs(GuestOs::Linux);
        assert_eq!(err.to_string(), "guest OS not supported by backend: Linux");
    }

    #[test]
    fn error_display_pattern_not_found() {
        let err = Error::PatternNotFound {
            pattern: "login:".to_string(),
        };
        assert_eq!(
            err.to_string(),
            "pattern not found in console output: login:"
        );
    }

    #[test]
    fn error_from_io_error() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file not found");
        let err: Error = io_err.into();
        assert!(matches!(err, Error::Io(_)));
        assert!(err.to_string().contains("file not found"));
    }
}
