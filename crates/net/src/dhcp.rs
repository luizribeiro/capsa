use heapless::Vec as HeaplessVec;
use smoltcp::wire::{DhcpMessageType, DhcpPacket, DhcpRepr, EthernetAddress, Ipv4Address};
use std::collections::HashMap;
use std::net::Ipv4Addr;

/// Simple DHCP server for assigning IPs to guest VMs.
pub struct DhcpServer {
    /// Our IP address (the gateway)
    server_ip: Ipv4Address,
    /// Subnet mask
    subnet_mask: Ipv4Address,
    /// Lease duration in seconds
    lease_duration: u32,
    /// Next IP to assign
    next_ip: Ipv4Addr,
    /// Last IP in the range
    last_ip: Ipv4Addr,
    /// Active leases: MAC -> IP
    leases: HashMap<EthernetAddress, Ipv4Address>,
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

        let mut dns_servers = HeaplessVec::new();
        dns_servers.push(Ipv4Address::new(8, 8, 8, 8)).ok();
        dns_servers.push(Ipv4Address::new(8, 8, 4, 4)).ok();

        Self {
            server_ip: gateway.into(),
            subnet_mask: mask.into(),
            lease_duration: 3600, // 1 hour
            next_ip: range_start,
            last_ip: range_end,
            leases: HashMap::new(),
            dns_servers,
        }
    }

    /// Handle an incoming DHCP packet and generate a response if needed.
    ///
    /// Returns `Some((response_repr, dest_ip))` if a response should be sent.
    pub fn handle_packet<'a>(
        &mut self,
        client_mac: EthernetAddress,
        packet: &DhcpPacket<&'a [u8]>,
    ) -> Option<DhcpRepr<'a>> {
        let repr = DhcpRepr::parse(packet).ok()?;

        match repr.message_type {
            DhcpMessageType::Discover => self.handle_discover(client_mac, &repr),
            DhcpMessageType::Request => self.handle_request(client_mac, &repr),
            DhcpMessageType::Release => {
                self.handle_release(client_mac);
                None
            }
            _ => None,
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
        // Check if we have a lease for this client
        let assigned_ip = self.leases.get(&client_mac).copied()?;

        // Verify the requested IP matches (if specified)
        if let Some(requested) = request.requested_ip {
            if requested != assigned_ip {
                return None; // NAK would be appropriate, but we'll just ignore
            }
        }

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

    fn handle_release(&mut self, client_mac: EthernetAddress) {
        // We don't reclaim IPs for simplicity (VM lifetimes are typically short)
        self.leases.remove(&client_mac);
    }

    fn get_or_allocate_ip(&mut self, client_mac: EthernetAddress) -> Option<Ipv4Address> {
        // Check for existing lease
        if let Some(&ip) = self.leases.get(&client_mac) {
            return Some(ip);
        }

        // Allocate new IP
        if self.next_ip > self.last_ip {
            tracing::warn!("DHCP pool exhausted");
            return None;
        }

        let ip: Ipv4Address = self.next_ip.into();
        self.leases.insert(client_mac, ip);

        // Increment next_ip
        let next = u32::from(self.next_ip) + 1;
        self.next_ip = Ipv4Addr::from(next);

        Some(ip)
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
}
