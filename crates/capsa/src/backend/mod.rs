mod vfkit;

pub(crate) use vfkit::VfkitBackend;

use crate::boot::KernelCmdline;
use crate::capabilities::BackendCapabilities;
use crate::error::Result;
use crate::types::{ConsoleMode, DiskImage, NetworkMode, ResourceConfig, SharedDir};
use async_trait::async_trait;
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite};

pub(crate) struct InternalVmConfig {
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    pub disk: Option<DiskImage>,
    pub cmdline: String,
    pub resources: ResourceConfig,
    pub shares: Vec<SharedDir>,
    pub network: NetworkMode,
    pub console: ConsoleMode,
}

pub(crate) type ConsoleStream = Box<dyn ConsoleIo + Send>;

pub(crate) trait ConsoleIo: AsyncRead + AsyncWrite + Unpin {}
impl<T: AsyncRead + AsyncWrite + Unpin> ConsoleIo for T {}

#[async_trait]
#[allow(dead_code)]
pub(crate) trait BackendVmHandle: Send + Sync {
    fn is_running(&self) -> bool;
    async fn wait(&self) -> Result<i32>;
    async fn shutdown(&self) -> Result<()>;
    async fn kill(&self) -> Result<()>;
    async fn console_stream(&self) -> Result<Option<ConsoleStream>>;
}

#[async_trait]
#[allow(dead_code)]
pub(crate) trait HypervisorBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn capabilities(&self) -> &BackendCapabilities;
    fn is_available(&self) -> bool;
    async fn start(&self, config: &InternalVmConfig) -> Result<Box<dyn BackendVmHandle>>;
    fn kernel_cmdline_defaults(&self) -> KernelCmdline;
    fn default_root_device(&self) -> &str;
}

pub(crate) fn select_backend() -> Result<Box<dyn HypervisorBackend>> {
    #[cfg(target_os = "macos")]
    {
        let vfkit = VfkitBackend::new();
        if vfkit.is_available() {
            return Ok(Box::new(vfkit));
        }
    }

    Err(crate::error::Error::NoBackendAvailable)
}
