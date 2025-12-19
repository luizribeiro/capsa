#[cfg(feature = "macos-native")]
mod native;
#[cfg(feature = "macos-subprocess")]
mod subprocess;
#[cfg(feature = "vfkit")]
mod vfkit;

use async_trait::async_trait;
use capsa_core::{BackendVmHandle, InternalVmConfig, Result};

#[cfg(feature = "macos-native")]
pub use native::NativeStrategy;
#[cfg(feature = "macos-subprocess")]
pub use subprocess::SubprocessStrategy;
#[cfg(feature = "vfkit")]
pub use vfkit::VfkitStrategy;

#[async_trait]
pub trait ExecutionStrategy: Send + Sync {
    fn name(&self) -> &'static str;
    fn is_available(&self) -> bool;
    async fn start(&self, config: &InternalVmConfig) -> Result<Box<dyn BackendVmHandle>>;
}
