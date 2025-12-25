//! Network cluster management for multi-VM networking.
//!
//! This module provides the high-level API for creating and managing network clusters
//! where multiple VMs can communicate with each other on a shared virtual switch.

use capsa_core::NetworkClusterConfig;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use tracing::info;

#[cfg(target_os = "macos")]
use capsa_core::{Error, Result};
#[cfg(target_os = "macos")]
use capsa_net::{ClusterStack, ClusterStackConfig, VirtualSwitch};
#[cfg(target_os = "macos")]
use nix::libc;
#[cfg(target_os = "macos")]
use std::os::fd::OwnedFd;
#[cfg(target_os = "macos")]
use std::sync::atomic::{AtomicBool, Ordering};

/// Global registry of network clusters.
static CLUSTERS: OnceLock<Mutex<HashMap<String, Arc<NetworkCluster>>>> = OnceLock::new();

fn clusters() -> &'static Mutex<HashMap<String, Arc<NetworkCluster>>> {
    CLUSTERS.get_or_init(|| Mutex::new(HashMap::new()))
}

/// A network cluster that multiple VMs can join.
///
/// Each cluster contains a virtual switch that forwards frames between VMs.
/// VMs get IP addresses via DHCP from the cluster's DHCP server.
pub struct NetworkCluster {
    config: NetworkClusterConfig,
    #[cfg(target_os = "macos")]
    switch: VirtualSwitch,
    #[cfg(target_os = "macos")]
    dhcp_started: AtomicBool,
}

impl NetworkCluster {
    /// Create a new network cluster with the given configuration.
    #[cfg(target_os = "macos")]
    pub fn create(config: NetworkClusterConfig) -> Arc<Self> {
        let switch = VirtualSwitch::new();
        info!(name = %config.name, subnet = %config.subnet, "Created network cluster");

        let cluster = Arc::new(Self {
            config,
            switch,
            dhcp_started: AtomicBool::new(false),
        });

        // Register in the global registry
        let mut clusters = clusters().lock().unwrap();
        clusters.insert(cluster.config.name.clone(), cluster.clone());

        cluster
    }

    /// Create a new network cluster with the given configuration.
    #[cfg(not(target_os = "macos"))]
    pub fn create(config: NetworkClusterConfig) -> Arc<Self> {
        info!(name = %config.name, subnet = %config.subnet, "Created network cluster");

        let cluster = Arc::new(Self { config });

        // Register in the global registry
        let mut clusters = clusters().lock().unwrap();
        clusters.insert(cluster.config.name.clone(), cluster.clone());

        cluster
    }

    /// Get an existing cluster by name, or create it with default config if it doesn't exist.
    #[cfg(target_os = "macos")]
    pub fn get_or_create(name: &str) -> Arc<Self> {
        let mut clusters = clusters().lock().unwrap();

        if let Some(cluster) = clusters.get(name) {
            return cluster.clone();
        }

        let config = NetworkClusterConfig {
            name: name.to_string(),
            ..Default::default()
        };

        let switch = VirtualSwitch::new();
        info!(name = %config.name, subnet = %config.subnet, "Created network cluster");

        let cluster = Arc::new(Self {
            config,
            switch,
            dhcp_started: AtomicBool::new(false),
        });

        clusters.insert(name.to_string(), cluster.clone());
        cluster
    }

    /// Get an existing cluster by name, or create it with default config if it doesn't exist.
    #[cfg(not(target_os = "macos"))]
    pub fn get_or_create(name: &str) -> Arc<Self> {
        let mut clusters = clusters().lock().unwrap();

        if let Some(cluster) = clusters.get(name) {
            return cluster.clone();
        }

        let config = NetworkClusterConfig {
            name: name.to_string(),
            ..Default::default()
        };

        info!(name = %config.name, subnet = %config.subnet, "Created network cluster");

        let cluster = Arc::new(Self { config });
        clusters.insert(name.to_string(), cluster.clone());

        cluster
    }

    #[cfg(target_os = "macos")]
    async fn ensure_dhcp_started(&self) {
        if self
            .dhcp_started
            .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
            .is_ok()
        {
            let stack_config =
                ClusterStackConfig::from_subnet(&self.config.subnet, self.config.gateway)
                    .unwrap_or_default();

            let port = self.switch.create_port().await;
            let cluster_name = self.config.name.clone();

            tokio::spawn(async move {
                info!(cluster = %cluster_name, "Started DHCP server for cluster");
                let stack = ClusterStack::new(port, stack_config);
                stack.run().await;
            });
        }
    }

    /// Get the cluster configuration.
    pub fn config(&self) -> &NetworkClusterConfig {
        &self.config
    }

    /// Create a new port on this cluster for a VM.
    ///
    /// Returns `(host_fd, guest_fd)` where:
    /// - `host_fd` should be used with `bridge_to_switch` to connect to the switch
    /// - `guest_fd` should be passed to the VM's network device
    #[cfg(target_os = "macos")]
    pub async fn create_port(&self) -> Result<ClusterPort> {
        use std::os::fd::{FromRawFd, RawFd};

        // Start DHCP server if not already running
        self.ensure_dhcp_started().await;

        let switch_port = self.switch.create_port().await;
        let port_id = switch_port.id();

        // Create socketpair for this VM
        let mut fds: [RawFd; 2] = [-1, -1];
        let result =
            unsafe { libc::socketpair(libc::AF_UNIX, libc::SOCK_DGRAM, 0, fds.as_mut_ptr()) };

        if result < 0 {
            return Err(Error::StartFailed(format!(
                "Failed to create socketpair: {}",
                std::io::Error::last_os_error()
            )));
        }

        let host_fd = unsafe { OwnedFd::from_raw_fd(fds[0]) };
        let guest_fd = unsafe { OwnedFd::from_raw_fd(fds[1]) };

        info!(port_id, "Created cluster port");

        Ok(ClusterPort {
            port_id,
            host_fd,
            guest_fd,
            switch_port,
        })
    }

    /// Remove a cluster from the registry.
    pub fn remove(name: &str) {
        let mut clusters = clusters().lock().unwrap();
        clusters.remove(name);
    }
}

/// A port on a network cluster for a VM.
#[cfg(target_os = "macos")]
pub struct ClusterPort {
    /// Port ID on the switch.
    pub port_id: usize,
    /// Host-side file descriptor (for bridge_to_switch).
    pub host_fd: OwnedFd,
    /// Guest-side file descriptor (for VM network device).
    pub guest_fd: OwnedFd,
    /// The underlying switch port.
    pub switch_port: capsa_net::SwitchPort,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_cluster() {
        let config = NetworkClusterConfig::default();
        let cluster = NetworkCluster::create(config);
        assert_eq!(cluster.config().name, "default");
    }

    #[test]
    fn get_or_create_returns_same_cluster() {
        let cluster1 = NetworkCluster::get_or_create("test-cluster");
        let cluster2 = NetworkCluster::get_or_create("test-cluster");

        // Both should point to the same cluster
        assert!(Arc::ptr_eq(&cluster1, &cluster2));

        // Cleanup
        NetworkCluster::remove("test-cluster");
    }
}
