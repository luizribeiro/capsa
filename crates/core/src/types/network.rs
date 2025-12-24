use serde::{Deserialize, Serialize};
use std::net::Ipv4Addr;

/// Network protocol for port forwarding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Protocol {
    Tcp,
    Udp,
}

/// Port forwarding rule: host_port → guest_port.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortForward {
    pub protocol: Protocol,
    pub host_port: u16,
    pub guest_port: u16,
}

/// Network configuration for VMs.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum NetworkMode {
    /// No network access.
    None,
    /// Platform-native NAT networking (macOS VZ built-in).
    #[default]
    Nat,
    /// Userspace NAT via capsa-net (cross-platform, supports filtering).
    #[serde(rename = "user_nat")]
    UserNat(UserNatConfig),
}

impl NetworkMode {
    /// Create a userspace NAT configuration with default settings.
    pub fn user_nat() -> UserNatConfigBuilder {
        UserNatConfigBuilder::default()
    }
}

/// Configuration for userspace NAT networking.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserNatConfig {
    /// Subnet for the guest network (e.g., "10.0.2.0/24").
    /// Default: 10.0.2.0/24
    pub subnet: String,
    /// Gateway IP within the subnet.
    /// Default: 10.0.2.2
    pub gateway: Ipv4Addr,
    /// First IP to assign via DHCP.
    /// Default: 10.0.2.15
    pub dhcp_start: Ipv4Addr,
    /// Last IP to assign via DHCP.
    /// Default: 10.0.2.254
    pub dhcp_end: Ipv4Addr,
    /// Port forwarding rules (host → guest).
    #[serde(default)]
    pub port_forwards: Vec<PortForward>,
}

impl Default for UserNatConfig {
    fn default() -> Self {
        Self {
            subnet: "10.0.2.0/24".to_string(),
            gateway: Ipv4Addr::new(10, 0, 2, 2),
            dhcp_start: Ipv4Addr::new(10, 0, 2, 15),
            dhcp_end: Ipv4Addr::new(10, 0, 2, 254),
            port_forwards: Vec::new(),
        }
    }
}

/// Builder for UserNatConfig.
#[derive(Debug, Clone, Default)]
pub struct UserNatConfigBuilder {
    config: UserNatConfig,
}

impl UserNatConfigBuilder {
    /// Set the subnet (e.g., "192.168.100.0/24").
    pub fn subnet(mut self, subnet: &str) -> Self {
        self.config.subnet = subnet.to_string();
        // Parse to update gateway and DHCP range
        if let Some((base, _prefix)) = subnet.split_once('/')
            && let Ok(base_ip) = base.parse::<Ipv4Addr>()
        {
            let octets = base_ip.octets();
            self.config.gateway = Ipv4Addr::new(octets[0], octets[1], octets[2], 2);
            self.config.dhcp_start = Ipv4Addr::new(octets[0], octets[1], octets[2], 15);
            self.config.dhcp_end = Ipv4Addr::new(octets[0], octets[1], octets[2], 254);
        }
        self
    }

    /// Forward a TCP port from host to guest.
    pub fn forward_tcp(mut self, host_port: u16, guest_port: u16) -> Self {
        self.config.port_forwards.push(PortForward {
            protocol: Protocol::Tcp,
            host_port,
            guest_port,
        });
        self
    }

    /// Forward a UDP port from host to guest.
    pub fn forward_udp(mut self, host_port: u16, guest_port: u16) -> Self {
        self.config.port_forwards.push(PortForward {
            protocol: Protocol::Udp,
            host_port,
            guest_port,
        });
        self
    }

    /// Build the NetworkMode.
    pub fn build(self) -> NetworkMode {
        NetworkMode::UserNat(self.config)
    }
}

impl From<UserNatConfigBuilder> for NetworkMode {
    fn from(builder: UserNatConfigBuilder) -> Self {
        builder.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_is_nat() {
        assert_eq!(NetworkMode::default(), NetworkMode::Nat);
    }

    #[test]
    fn serializes_lowercase() {
        assert_eq!(
            serde_json::to_string(&NetworkMode::None).unwrap(),
            "\"none\""
        );
        assert_eq!(serde_json::to_string(&NetworkMode::Nat).unwrap(), "\"nat\"");
    }

    #[test]
    fn deserializes_lowercase() {
        assert_eq!(
            serde_json::from_str::<NetworkMode>("\"none\"").unwrap(),
            NetworkMode::None
        );
        assert_eq!(
            serde_json::from_str::<NetworkMode>("\"nat\"").unwrap(),
            NetworkMode::Nat
        );
    }

    #[test]
    fn user_nat_default_config() {
        let config = UserNatConfig::default();
        assert_eq!(config.subnet, "10.0.2.0/24");
        assert_eq!(config.gateway, Ipv4Addr::new(10, 0, 2, 2));
        assert_eq!(config.dhcp_start, Ipv4Addr::new(10, 0, 2, 15));
        assert_eq!(config.dhcp_end, Ipv4Addr::new(10, 0, 2, 254));
    }

    #[test]
    fn user_nat_builder() {
        let mode = NetworkMode::user_nat().subnet("192.168.1.0/24").build();
        match mode {
            NetworkMode::UserNat(config) => {
                assert_eq!(config.subnet, "192.168.1.0/24");
                assert_eq!(config.gateway, Ipv4Addr::new(192, 168, 1, 2));
            }
            _ => panic!("Expected UserNat"),
        }
    }

    #[test]
    fn user_nat_into_network_mode() {
        let mode: NetworkMode = NetworkMode::user_nat().into();
        assert!(matches!(mode, NetworkMode::UserNat(_)));
    }

    #[test]
    fn user_nat_port_forwards() {
        let mode = NetworkMode::user_nat()
            .forward_tcp(8080, 80)
            .forward_udp(5353, 53)
            .build();
        match mode {
            NetworkMode::UserNat(config) => {
                assert_eq!(config.port_forwards.len(), 2);
                assert_eq!(config.port_forwards[0].protocol, Protocol::Tcp);
                assert_eq!(config.port_forwards[0].host_port, 8080);
                assert_eq!(config.port_forwards[0].guest_port, 80);
                assert_eq!(config.port_forwards[1].protocol, Protocol::Udp);
                assert_eq!(config.port_forwards[1].host_port, 5353);
                assert_eq!(config.port_forwards[1].guest_port, 53);
            }
            _ => panic!("Expected UserNat"),
        }
    }
}
