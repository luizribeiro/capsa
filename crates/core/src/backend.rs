use crate::boot::KernelCmdline;
use crate::capabilities::BackendCapabilities;
use crate::error::Result;
use crate::types::{DiskImage, HostPlatform, NetworkMode, ResourceConfig, SharedDir};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use tokio::io::{AsyncRead, AsyncWrite};

/// Boot method configuration for a VM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BootMethod {
    /// Linux direct kernel boot - bypasses bootloader for faster boot.
    LinuxDirect {
        kernel: PathBuf,
        initrd: PathBuf,
        cmdline: String,
    },
    /// UEFI boot - uses EFI bootloader on disk (OS-agnostic).
    Uefi {
        efi_variable_store: PathBuf,
        create_variable_store: bool,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VmConfig {
    pub boot: BootMethod,
    pub root_disk: Option<DiskImage>,
    #[serde(default)]
    pub disks: Vec<DiskImage>,
    pub resources: ResourceConfig,
    pub shares: Vec<SharedDir>,
    pub network: NetworkMode,
    pub console_enabled: bool,
}

pub type ConsoleStream = Box<dyn ConsoleIo + Send>;

pub trait ConsoleIo: AsyncRead + AsyncWrite + Unpin {}
impl<T: AsyncRead + AsyncWrite + Unpin> ConsoleIo for T {}

#[async_trait]
pub trait BackendVmHandle: Send + Sync {
    async fn is_running(&self) -> bool;
    async fn wait(&self) -> Result<i32>;
    // TODO: better investigate how shutdown is handling ACPI, timeouts, etc
    async fn shutdown(&self) -> Result<()>;
    async fn kill(&self) -> Result<()>;
    async fn console_stream(&self) -> Result<Option<ConsoleStream>>;
}

#[async_trait]
pub trait HypervisorBackend: Send + Sync {
    fn name(&self) -> &'static str;
    fn platform(&self) -> HostPlatform;
    fn capabilities(&self) -> &BackendCapabilities;
    fn is_available(&self) -> bool;
    async fn start(&self, config: &VmConfig) -> Result<Box<dyn BackendVmHandle>>;
    fn kernel_cmdline_defaults(&self) -> KernelCmdline;
    fn default_root_device(&self) -> &str;
}
