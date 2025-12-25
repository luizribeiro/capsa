use crate::cluster::NetworkCluster;
use async_trait::async_trait;
use capsa_core::{
    BackendCapabilities, BackendVmHandle, ConsoleStream, HostPlatform, HypervisorBackend,
    KernelCmdline, NetworkMode, Result, VmConfig,
};
use capsa_net::SwitchPort;
use std::os::fd::{IntoRawFd, OwnedFd};
use tokio::task::JoinHandle;

struct ClusterPortInfo {
    host_fd: OwnedFd,
    switch_port: SwitchPort,
}

/// Linux KVM backend wrapper that handles cluster networking.
///
/// This wraps the underlying KvmBackend to add cluster networking support
/// by creating cluster ports and spawning bridge tasks.
pub struct LinuxKvmBackend {
    inner: capsa_linux_kvm::KvmBackend,
}

impl LinuxKvmBackend {
    pub fn new() -> Self {
        Self {
            inner: capsa_linux_kvm::KvmBackend::new(),
        }
    }
}

impl Default for LinuxKvmBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HypervisorBackend for LinuxKvmBackend {
    fn name(&self) -> &'static str {
        self.inner.name()
    }

    fn is_available(&self) -> bool {
        self.inner.is_available()
    }

    fn capabilities(&self) -> &BackendCapabilities {
        self.inner.capabilities()
    }

    fn kernel_cmdline_defaults(&self) -> KernelCmdline {
        self.inner.kernel_cmdline_defaults()
    }

    fn platform(&self) -> HostPlatform {
        self.inner.platform()
    }

    fn default_root_device(&self) -> &str {
        self.inner.default_root_device()
    }

    async fn start(&self, config: &VmConfig) -> Result<Box<dyn BackendVmHandle>> {
        // Create cluster port if cluster networking is configured
        let (config, cluster_port) =
            if let NetworkMode::Cluster(ref cluster_config) = config.network {
                tracing::info!(
                    cluster = %cluster_config.cluster_name,
                    "Creating cluster port for VM"
                );
                let cluster = NetworkCluster::get_or_create(&cluster_config.cluster_name);
                let port = cluster.create_port().await?;
                tracing::info!(port_id = port.port_id, "Cluster port created");

                // Transfer ownership of guest_fd to the inner backend via raw fd
                let mut config = config.clone();
                config.cluster_network_fd = Some(port.guest_fd.into_raw_fd());

                (
                    config,
                    Some(ClusterPortInfo {
                        host_fd: port.host_fd,
                        switch_port: port.switch_port,
                    }),
                )
            } else {
                (config.clone(), None)
            };

        // Start the underlying backend
        let inner_handle = self.inner.start(&config).await?;

        // Spawn bridge task for cluster networking
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

        Ok(Box::new(KvmVmHandle {
            inner: inner_handle,
            _bridge_task: bridge_task,
        }))
    }
}

struct KvmVmHandle {
    inner: Box<dyn BackendVmHandle>,
    #[allow(dead_code)]
    _bridge_task: Option<JoinHandle<()>>,
}

#[async_trait]
impl BackendVmHandle for KvmVmHandle {
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
