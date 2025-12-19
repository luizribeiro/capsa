use super::ExecutionStrategy;
use async_trait::async_trait;
use capsa_backend_native::NativeVirtualizationBackend;
use capsa_core::{BackendVmHandle, HypervisorBackend, Result, VmConfig};

pub struct NativeStrategy {
    backend: NativeVirtualizationBackend,
}

impl NativeStrategy {
    pub fn new() -> Self {
        Self {
            backend: NativeVirtualizationBackend::new(),
        }
    }
}

impl Default for NativeStrategy {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionStrategy for NativeStrategy {
    fn name(&self) -> &'static str {
        self.backend.name()
    }

    fn is_available(&self) -> bool {
        self.backend.is_available()
    }

    async fn start(&self, config: &VmConfig) -> Result<Box<dyn BackendVmHandle>> {
        self.backend.start(config).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod strategy_construction {
        use super::*;

        #[test]
        fn new_creates_strategy() {
            let strategy = NativeStrategy::new();
            assert_eq!(strategy.name(), "native-virtualization");
        }

        #[test]
        fn default_creates_same_as_new() {
            let strategy1 = NativeStrategy::new();
            let strategy2 = NativeStrategy::default();
            assert_eq!(strategy1.name(), strategy2.name());
        }
    }
}
