use heapless::Vec as HeaplessVec;
use smoltcp::wire::{DhcpMessageType, DhcpPacket, DhcpRepr, EthernetAddress, Ipv4Address};
use std::collections::HashMap;
use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

pub(super) enum DhcpEvent<'a> {
    Response(DhcpRepr<'a>),
    Released(EthernetAddress),
    None,
}

/// Simple DHCP server for assigning IPs to guest VMs.
pub struct DhcpServer {
    /// Our IP address (the gateway)
    server_ip: Ipv4Address,
    /// Subnet mask
    subnet_mask: Ipv4Address,
    /// Lease duration in seconds
    lease_duration: u32,
    /// First IP in the assignable range (for backfill scanning)
    first_ip: Ipv4Addr,
    /// Next IP to assign
    next_ip: Ipv4Addr,
    /// Last IP in the range
    last_ip: Ipv4Addr,
    /// Active leases: MAC -> (IP, last confirmed timestamp)
    leases: HashMap<EthernetAddress, (Ipv4Address, Instant)>,
    /// DNS servers to advertise (max 3 per smoltcp)
    dns_servers: HeaplessVec<Ipv4Address, 3>,
}

impl DhcpServer {
    /// Create a new DHCP server for the given subnet.
    ///
    /// - `gateway`: The gateway IP (our IP), e.g., 10.0.2.2
    /// - `subnet_prefix`: The subnet prefix length, e.g., 24 for /24
    /// - `range_start`: First IP to assign, e.g., 10.0.2.15
    /// - `range_end`: Last IP to assign, e.g., 10.0.2.254
    pub fn new(
        gateway: Ipv4Addr,
        subnet_prefix: u8,
        range_start: Ipv4Addr,
        range_end: Ipv4Addr,
    ) -> Self {
        let mask = prefix_to_mask(subnet_prefix);

        // Point DNS to the gateway (our DNS proxy)
        let mut dns_servers = HeaplessVec::new();
        dns_servers.push(gateway).ok();

        Self {
            server_ip: gateway,
            subnet_mask: mask,
            lease_duration: 3600, // 1 hour
            first_ip: range_start,
            next_ip: range_start,
            last_ip: range_end,
            leases: HashMap::new(),
            dns_servers,
        }
    }

    pub(super) fn handle_packet<'a>(
        &mut self,
        client_mac: EthernetAddress,
        packet: &DhcpPacket<&'a [u8]>,
    ) -> DhcpEvent<'a> {
        let Some(repr) = DhcpRepr::parse(packet).ok() else {
            return DhcpEvent::None;
        };

        match repr.message_type {
            DhcpMessageType::Discover => match self.handle_discover(client_mac, &repr) {
                Some(r) => DhcpEvent::Response(r),
                None => DhcpEvent::None,
            },
            DhcpMessageType::Request => match self.handle_request(client_mac, &repr) {
                Some(r) => DhcpEvent::Response(r),
                None => DhcpEvent::None,
            },
            DhcpMessageType::Release => {
                self.handle_release(client_mac);
                DhcpEvent::Released(client_mac)
            }
            _ => DhcpEvent::None,
        }
    }

    fn handle_discover<'a>(
        &mut self,
        client_mac: EthernetAddress,
        request: &DhcpRepr<'_>,
    ) -> Option<DhcpRepr<'a>> {
        let offered_ip = self.get_or_allocate_ip(client_mac)?;

        Some(DhcpRepr {
            message_type: DhcpMessageType::Offer,
            transaction_id: request.transaction_id,
            secs: 0,
            client_hardware_address: client_mac,
            client_ip: Ipv4Address::UNSPECIFIED,
            your_ip: offered_ip,
            server_ip: self.server_ip,
            router: Some(self.server_ip),
            subnet_mask: Some(self.subnet_mask),
            relay_agent_ip: Ipv4Address::UNSPECIFIED,
            broadcast: true,
            requested_ip: None,
            client_identifier: None,
            server_identifier: Some(self.server_ip),
            parameter_request_list: None,
            dns_servers: Some(self.dns_servers.clone()),
            max_size: None,
            lease_duration: Some(self.lease_duration),
            renew_duration: Some(self.lease_duration / 2),
            rebind_duration: Some(self.lease_duration * 7 / 8),
            additional_options: &[],
        })
    }

    fn handle_request<'a>(
        &mut self,
        client_mac: EthernetAddress,
        request: &DhcpRepr<'_>,
    ) -> Option<DhcpRepr<'a>> {
        let (assigned_ip, _) = self.leases.get(&client_mac).copied()?;

        if let Some(requested) = request.requested_ip {
            if requested != assigned_ip {
                return None; // NAK would be appropriate, but we'll just ignore
            }
        }

        // Refresh timestamp on ACK
        self.leases
            .insert(client_mac, (assigned_ip, Instant::now()));

        Some(DhcpRepr {
            message_type: DhcpMessageType::Ack,
            transaction_id: request.transaction_id,
            secs: 0,
            client_hardware_address: client_mac,
            client_ip: Ipv4Address::UNSPECIFIED,
            your_ip: assigned_ip,
            server_ip: self.server_ip,
            router: Some(self.server_ip),
            subnet_mask: Some(self.subnet_mask),
            relay_agent_ip: Ipv4Address::UNSPECIFIED,
            broadcast: true,
            requested_ip: None,
            client_identifier: None,
            server_identifier: Some(self.server_ip),
            parameter_request_list: None,
            dns_servers: Some(self.dns_servers.clone()),
            max_size: None,
            lease_duration: Some(self.lease_duration),
            renew_duration: Some(self.lease_duration / 2),
            rebind_duration: Some(self.lease_duration * 7 / 8),
            additional_options: &[],
        })
    }

    #[cfg(test)]
    pub fn lookup_ip(&self, mac: &[u8; 6]) -> Option<Ipv4Addr> {
        let eth_addr = EthernetAddress(*mac);
        self.leases.get(&eth_addr).map(|(ip, _)| (*ip).into())
    }

    #[cfg(test)]
    pub fn get_or_allocate_ip_for_test(&mut self, mac: EthernetAddress) -> Option<Ipv4Address> {
        self.get_or_allocate_ip(mac)
    }

    #[cfg(test)]
    pub fn backdate_lease_for_test(&mut self, mac: &[u8; 6], age: Duration) {
        let eth_addr = EthernetAddress(*mac);
        if let Some(entry) = self.leases.get_mut(&eth_addr) {
            entry.1 = Instant::now() - age;
        }
    }

    fn handle_release(&mut self, client_mac: EthernetAddress) {
        self.leases.remove(&client_mac);
    }

    pub fn cleanup_expired(&mut self, timeout: Duration) -> Vec<[u8; 6]> {
        let now = Instant::now();
        let mut expired = Vec::new();

        self.leases.retain(|mac, (_, confirmed_at)| {
            if now.saturating_duration_since(*confirmed_at) > timeout {
                expired.push(mac.0);
                false
            } else {
                true
            }
        });

        expired
    }

    fn get_or_allocate_ip(&mut self, client_mac: EthernetAddress) -> Option<Ipv4Address> {
        if let Some(&(ip, _)) = self.leases.get(&client_mac) {
            return Some(ip);
        }

        let now = Instant::now();

        if self.next_ip <= self.last_ip {
            let ip: Ipv4Address = self.next_ip;
            self.leases.insert(client_mac, (ip, now));
            let next = u32::from(self.next_ip) + 1;
            self.next_ip = Ipv4Addr::from(next);
            return Some(ip);
        }

        // next_ip exhausted — scan the range for a freed IP
        let allocated_ips: std::collections::HashSet<Ipv4Address> =
            self.leases.values().map(|(ip, _)| *ip).collect();

        let mut candidate = u32::from(self.first_ip);
        let end = u32::from(self.last_ip);
        while candidate <= end {
            let ip: Ipv4Address = Ipv4Addr::from(candidate);
            if !allocated_ips.contains(&ip) {
                self.leases.insert(client_mac, (ip, now));
                return Some(ip);
            }
            candidate += 1;
        }

        tracing::warn!("DHCP pool exhausted");
        None
    }
}

/// Convert a prefix length to a subnet mask.
fn prefix_to_mask(prefix: u8) -> Ipv4Addr {
    if prefix == 0 {
        Ipv4Addr::new(0, 0, 0, 0)
    } else if prefix >= 32 {
        Ipv4Addr::new(255, 255, 255, 255)
    } else {
        let mask = !((1u32 << (32 - prefix)) - 1);
        Ipv4Addr::from(mask)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_prefix_to_mask() {
        assert_eq!(prefix_to_mask(24), Ipv4Addr::new(255, 255, 255, 0));
        assert_eq!(prefix_to_mask(16), Ipv4Addr::new(255, 255, 0, 0));
        assert_eq!(prefix_to_mask(8), Ipv4Addr::new(255, 0, 0, 0));
        assert_eq!(prefix_to_mask(32), Ipv4Addr::new(255, 255, 255, 255));
        assert_eq!(prefix_to_mask(0), Ipv4Addr::new(0, 0, 0, 0));
    }

    #[test]
    fn lookup_ip_returns_none_for_unknown_mac() {
        let server = DhcpServer::new(
            Ipv4Addr::new(10, 0, 2, 1),
            24,
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(10, 0, 2, 254),
        );
        assert!(server
            .lookup_ip(&[0x52, 0x54, 0x00, 0x00, 0x00, 0x01])
            .is_none());
    }

    #[test]
    fn lookup_ip_returns_ip_after_allocation() {
        let mut server = DhcpServer::new(
            Ipv4Addr::new(10, 0, 2, 1),
            24,
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(10, 0, 2, 254),
        );
        let mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        server.get_or_allocate_ip(mac);

        assert_eq!(
            server.lookup_ip(&[0x52, 0x54, 0x00, 0x00, 0x00, 0x01]),
            Some(Ipv4Addr::new(10, 0, 2, 15))
        );
    }

    #[test]
    fn expired_lease_removed_by_cleanup() {
        let mut server = DhcpServer::new(
            Ipv4Addr::new(10, 0, 2, 1),
            24,
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(10, 0, 2, 254),
        );
        let mac_bytes = [0x52, 0x54, 0x00, 0x00, 0x00, 0x01];
        let mac = EthernetAddress(mac_bytes);
        server.get_or_allocate_ip(mac);
        assert!(server.lookup_ip(&mac_bytes).is_some());

        // Backdate the lease timestamp so it appears expired
        let old_time = Instant::now() - Duration::from_secs(7200);
        if let Some(entry) = server.leases.get_mut(&mac) {
            entry.1 = old_time;
        }

        let freed = server.cleanup_expired(Duration::from_secs(3600));
        assert_eq!(freed, vec![mac_bytes]);
        assert!(server.lookup_ip(&mac_bytes).is_none());
    }

    #[test]
    fn recent_lease_survives_cleanup() {
        let mut server = DhcpServer::new(
            Ipv4Addr::new(10, 0, 2, 1),
            24,
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(10, 0, 2, 254),
        );
        let mac_bytes = [0x52, 0x54, 0x00, 0x00, 0x00, 0x01];
        let mac = EthernetAddress(mac_bytes);
        server.get_or_allocate_ip(mac);

        let freed = server.cleanup_expired(Duration::from_secs(3600));
        assert!(freed.is_empty());
        assert!(server.lookup_ip(&mac_bytes).is_some());
    }

    #[test]
    fn freed_ip_reused_after_next_ip_exhausted() {
        let mut server = DhcpServer::new(
            Ipv4Addr::new(10, 0, 2, 1),
            24,
            Ipv4Addr::new(10, 0, 2, 15),
            Ipv4Addr::new(10, 0, 2, 16),
        );

        let mac1 = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let mac2 = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);
        let mac3 = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x03]);

        // Allocate both IPs in the pool
        let ip1 = server.get_or_allocate_ip(mac1).unwrap();
        let ip2 = server.get_or_allocate_ip(mac2).unwrap();
        assert_eq!(Ipv4Addr::from(ip1), Ipv4Addr::new(10, 0, 2, 15));
        assert_eq!(Ipv4Addr::from(ip2), Ipv4Addr::new(10, 0, 2, 16));

        // next_ip is now past last_ip — pool appears exhausted
        assert!(server.next_ip > server.last_ip);

        // Release mac1's lease
        server.handle_release(mac1);

        // A new MAC should reuse the freed IP via backfill scan
        let ip3 = server.get_or_allocate_ip(mac3).unwrap();
        assert_eq!(Ipv4Addr::from(ip3), Ipv4Addr::new(10, 0, 2, 15));
    }
}
