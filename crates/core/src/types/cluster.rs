//! Network cluster configuration for multi-VM communication.

use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// Configuration for a network cluster (shared virtual switch).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NetworkClusterConfig {
    /// Cluster name for identification.
    pub name: String,
    /// Subnet for the cluster (e.g., "10.0.3.0/24").
    pub subnet: String,
    /// Gateway IP for external access (if NAT enabled).
    pub gateway: Option<Ipv4Addr>,
    /// Enable NAT for external connectivity.
    #[serde(default)]
    pub enable_nat: bool,
}

impl Default for NetworkClusterConfig {
    fn default() -> Self {
        Self {
            name: "default".to_string(),
            subnet: "10.0.3.0/24".to_string(),
            gateway: Some(Ipv4Addr::new(10, 0, 3, 1)),
            enable_nat: true,
        }
    }
}

/// Builder for NetworkClusterConfig.
#[derive(Debug, Clone, Default)]
pub struct NetworkClusterBuilder {
    config: NetworkClusterConfig,
}

impl NetworkClusterBuilder {
    /// Create a new cluster builder with the given name.
    pub fn new(name: &str) -> Self {
        Self {
            config: NetworkClusterConfig {
                name: name.to_string(),
                ..Default::default()
            },
        }
    }

    /// Set the subnet.
    pub fn subnet(mut self, subnet: &str) -> Self {
        self.config.subnet = subnet.to_string();
        // Update gateway based on subnet
        if let Some((base, _prefix)) = subnet.split_once('/')
            && let Ok(base_ip) = base.parse::<Ipv4Addr>()
        {
            let octets = base_ip.octets();
            self.config.gateway = Some(Ipv4Addr::new(octets[0], octets[1], octets[2], 1));
        }
        self
    }

    /// Disable NAT (isolated cluster).
    pub fn no_nat(mut self) -> Self {
        self.config.enable_nat = false;
        self.config.gateway = None;
        self
    }

    /// Build the configuration.
    pub fn build(self) -> NetworkClusterConfig {
        self.config
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_cluster() {
        let config = NetworkClusterConfig::default();
        assert_eq!(config.name, "default");
        assert_eq!(config.subnet, "10.0.3.0/24");
        assert!(config.enable_nat);
    }

    #[test]
    fn cluster_builder() {
        let config = NetworkClusterBuilder::new("test")
            .subnet("192.168.10.0/24")
            .build();
        assert_eq!(config.name, "test");
        assert_eq!(config.subnet, "192.168.10.0/24");
        assert_eq!(config.gateway, Some(Ipv4Addr::new(192, 168, 10, 1)));
    }

    #[test]
    fn isolated_cluster() {
        let config = NetworkClusterBuilder::new("isolated").no_nat().build();
        assert!(!config.enable_nat);
        assert!(config.gateway.is_none());
    }
}
