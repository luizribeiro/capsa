// TODO: make sure all capabilities are covered by tests using the minimal VM

mod capabilities;
mod cmdline;
pub(crate) mod pty;
mod strategy;

use async_trait::async_trait;
use capsa_core::{
    BackendCapabilities, BackendVmHandle, HostPlatform, HypervisorBackend, KernelCmdline, Result,
    VmConfig,
};

pub use capabilities::macos_virtualization_capabilities;
pub use cmdline::{DEFAULT_ROOT_DEVICE, macos_cmdline_defaults};
use strategy::ExecutionStrategy;
#[cfg(feature = "macos-native")]
pub use strategy::NativeStrategy;
#[cfg(feature = "macos-subprocess")]
pub use strategy::SubprocessStrategy;
#[cfg(feature = "vfkit")]
pub use strategy::VfkitStrategy;

pub struct MacOsBackend {
    strategy: Box<dyn ExecutionStrategy>,
    capabilities: BackendCapabilities,
}

impl MacOsBackend {
    pub fn new(strategy: Box<dyn ExecutionStrategy>) -> Self {
        Self {
            strategy,
            capabilities: macos_virtualization_capabilities(),
        }
    }

    #[cfg(feature = "vfkit")]
    pub fn vfkit() -> Self {
        Self::new(Box::new(VfkitStrategy::new()))
    }

    #[cfg(feature = "macos-subprocess")]
    pub fn subprocess() -> Self {
        Self::new(Box::new(SubprocessStrategy::new()))
    }

    #[cfg(feature = "macos-native")]
    pub fn native() -> Self {
        Self::new(Box::new(NativeStrategy::new()))
    }
}

#[async_trait]
impl HypervisorBackend for MacOsBackend {
    fn name(&self) -> &'static str {
        self.strategy.name()
    }

    fn platform(&self) -> HostPlatform {
        HostPlatform::MacOs
    }

    fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    fn is_available(&self) -> bool {
        self.strategy.is_available()
    }

    async fn start(&self, config: &VmConfig) -> Result<Box<dyn BackendVmHandle>> {
        self.strategy.start(config).await
    }

    fn kernel_cmdline_defaults(&self) -> KernelCmdline {
        macos_cmdline_defaults()
    }

    fn default_root_device(&self) -> &str {
        DEFAULT_ROOT_DEVICE
    }
}
