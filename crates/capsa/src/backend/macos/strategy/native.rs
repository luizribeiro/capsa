use super::ExecutionStrategy;
use crate::cluster::NetworkCluster;
use async_trait::async_trait;
use capsa_apple_vz::NativeVirtualizationBackend;
use capsa_core::{
    BackendVmHandle, ConsoleStream, HypervisorBackend, NetworkMode, Result, VmConfig,
};
use capsa_net::SwitchPort;
use std::os::fd::{AsRawFd, OwnedFd};
use tokio::task::JoinHandle;

struct ClusterPortInfo {
    guest_fd: OwnedFd,
    host_fd: OwnedFd,
    switch_port: SwitchPort,
}

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
        let cluster_port: Option<ClusterPortInfo> =
            if let NetworkMode::Cluster(ref cluster_config) = config.network {
                let cluster = NetworkCluster::get_or_create(&cluster_config.cluster_name);
                let port = cluster.create_port().await?;
                Some(ClusterPortInfo {
                    guest_fd: port.guest_fd,
                    host_fd: port.host_fd,
                    switch_port: port.switch_port,
                })
            } else {
                None
            };

        let mut config = config.clone();
        if let Some(ref port) = cluster_port {
            config.cluster_network_fd = Some(port.guest_fd.as_raw_fd());
        }

        let inner_handle = self.backend.start(&config).await?;

        let bridge_task = if let Some(port) = cluster_port {
            use capsa_net::bridge_to_switch;

            Some(tokio::spawn(async move {
                if let Err(e) = bridge_to_switch(port.host_fd, port.switch_port).await {
                    tracing::error!(error = %e, "Cluster bridge error");
                }
            }))
        } else {
            None
        };

        Ok(Box::new(NativeVmHandle {
            inner: inner_handle,
            _bridge_task: bridge_task,
        }))
    }
}

struct NativeVmHandle {
    inner: Box<dyn BackendVmHandle>,
    #[allow(dead_code)]
    _bridge_task: Option<JoinHandle<()>>,
}

#[async_trait]
impl BackendVmHandle for NativeVmHandle {
    async fn is_running(&self) -> bool {
        self.inner.is_running().await
    }

    async fn wait(&self) -> Result<i32> {
        self.inner.wait().await
    }

    async fn shutdown(&self) -> Result<()> {
        self.inner.shutdown().await
    }

    async fn kill(&self) -> Result<()> {
        self.inner.kill().await
    }

    async fn console_stream(&self) -> Result<Option<ConsoleStream>> {
        self.inner.console_stream().await
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
