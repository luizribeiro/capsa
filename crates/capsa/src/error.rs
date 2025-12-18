use crate::types::GuestOs;

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

    #[error("operation timed out")]
    Timeout,

    #[error("pattern not found in console output: {pattern}")]
    PatternNotFound { pattern: String },

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
