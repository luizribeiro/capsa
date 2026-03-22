use std::net::Ipv4Addr;
use std::time::Duration;

/// How often to run NAT cleanup and DNS cache expiry.
pub(super) const CLEANUP_INTERVAL: Duration = Duration::from_secs(10);

/// How long a DHCP lease can go unconfirmed before being expired.
pub(super) const DHCP_LEASE_TIMEOUT: Duration = Duration::from_secs(3600);

/// Check if an IP is external (should be NAT'd) given network config.
/// Returns true for routable external IPs, false for gateway, broadcast, and multicast.
pub(super) fn is_ip_external(dst_ip: Ipv4Addr, gateway_ip: Ipv4Addr, subnet_prefix: u8) -> bool {
    if dst_ip == gateway_ip {
        return false;
    }

    if dst_ip.is_broadcast() {
        return false;
    }

    if dst_ip.is_multicast() {
        return false;
    }

    if subnet_prefix == 0 {
        return true;
    }
    if subnet_prefix >= 32 {
        return false;
    }

    let subnet_mask = !((1u32 << (32 - subnet_prefix)) - 1);
    let subnet = u32::from_be_bytes(gateway_ip.octets()) & subnet_mask;
    let dst = u32::from_be_bytes(dst_ip.octets());
    let broadcast = subnet | !subnet_mask;
    if dst == broadcast {
        return false;
    }

    // Internal subnet unicast should stay on the local network.
    if (dst & subnet_mask) == subnet {
        return false;
    }

    true
}

/// Configuration for the gateway stack.
#[derive(Clone, Debug)]
pub struct GatewayStackConfig {
    /// Gateway IP address (our IP)
    pub gateway_ip: Ipv4Addr,
    /// Subnet prefix length
    pub subnet_prefix: u8,
    /// First IP to assign via DHCP
    pub dhcp_range_start: Ipv4Addr,
    /// Last IP to assign via DHCP
    pub dhcp_range_end: Ipv4Addr,
    /// MAC address for the gateway interface
    pub gateway_mac: [u8; 6],
}

impl Default for GatewayStackConfig {
    fn default() -> Self {
        Self {
            gateway_ip: Ipv4Addr::new(10, 0, 2, 2),
            subnet_prefix: 24,
            dhcp_range_start: Ipv4Addr::new(10, 0, 2, 15),
            dhcp_range_end: Ipv4Addr::new(10, 0, 2, 254),
            gateway_mac: [0x52, 0x54, 0x00, 0x00, 0x00, 0x01],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_ip_external_subnet_broadcast() {
        assert!(!is_ip_external(
            Ipv4Addr::new(10, 0, 2, 255),
            Ipv4Addr::new(10, 0, 2, 2),
            24
        ));
    }

    #[test]
    fn test_is_ip_external_gateway() {
        assert!(!is_ip_external(
            Ipv4Addr::new(10, 0, 2, 2),
            Ipv4Addr::new(10, 0, 2, 2),
            24
        ));
    }

    #[test]
    fn test_is_ip_external_normal_external() {
        assert!(is_ip_external(
            Ipv4Addr::new(8, 8, 8, 8),
            Ipv4Addr::new(10, 0, 2, 2),
            24
        ));
    }

    #[test]
    fn test_is_ip_external_same_subnet_host_is_internal() {
        assert!(!is_ip_external(
            Ipv4Addr::new(10, 0, 2, 42),
            Ipv4Addr::new(10, 0, 2, 2),
            24
        ));
    }
}
