use crate::frame::parse::parse_ipv4_frame;

use super::config::{is_ip_external, GatewayStackConfig};
use super::dns::DnsQueryInfo;
use super::outbound_buffer::OutboundFrameBuffer;
use super::GatewayStack;

use std::net::{Ipv4Addr, SocketAddrV4};

use super::tcp::{new_smoltcp_tcp_socket, InitiateResult};

use smoltcp::wire::{
    ArpOperation, ArpPacket, EthernetAddress, EthernetFrame, EthernetProtocol, IpAddress,
    IpListenEndpoint, IpProtocol, Ipv4Packet, TcpPacket, UdpPacket,
};

struct ExternalTcpInfo {
    guest_src: SocketAddrV4,
    remote_dst: SocketAddrV4,
    is_syn: bool,
}

enum FrameClassification {
    DnsQuery(DnsQueryInfo),
    ExternalTcp(ExternalTcpInfo),
    ExternalOther,
    ForGateway,
    Ignored,
}

fn classify_frame(frame: &[u8], config: &GatewayStackConfig) -> FrameClassification {
    let Some((eth, ip)) = parse_ipv4_frame(frame) else {
        unreachable!("caller ensures frame is IPv4");
    };

    let dst_ip: Ipv4Addr = ip.dst_addr();
    let src_ip: Ipv4Addr = ip.src_addr();
    let src_mac = eth.src_addr();
    let gateway_ip = config.gateway_ip;
    let gateway_mac = EthernetAddress(config.gateway_mac);

    if is_dns_query_to_gateway(&ip, dst_ip, gateway_ip) {
        if let Some(info) = parse_dns_query(&ip, src_mac, gateway_ip) {
            return FrameClassification::DnsQuery(info);
        }
    }

    if is_ip_external(dst_ip, gateway_ip, config.subnet_prefix) {
        if ip.next_header() == IpProtocol::Tcp {
            if let Ok(tcp) = TcpPacket::new_checked(ip.payload()) {
                return FrameClassification::ExternalTcp(ExternalTcpInfo {
                    guest_src: SocketAddrV4::new(src_ip, tcp.src_port()),
                    remote_dst: SocketAddrV4::new(dst_ip, tcp.dst_port()),
                    is_syn: tcp.syn() && !tcp.ack(),
                });
            }
        }
        return FrameClassification::ExternalOther;
    }

    if is_mac_for_gateway(eth.dst_addr(), gateway_mac) {
        return FrameClassification::ForGateway;
    }

    FrameClassification::Ignored
}

fn is_dns_query_to_gateway(
    ip_packet: &Ipv4Packet<&[u8]>,
    dst_ip: Ipv4Addr,
    gateway_ip: Ipv4Addr,
) -> bool {
    dst_ip == gateway_ip
        && ip_packet.next_header() == IpProtocol::Udp
        && UdpPacket::new_checked(ip_packet.payload())
            .map(|udp| udp.dst_port() == 53)
            .unwrap_or(false)
}

fn parse_dns_query(
    ip_packet: &Ipv4Packet<&[u8]>,
    guest_mac: EthernetAddress,
    gateway_ip: Ipv4Addr,
) -> Option<DnsQueryInfo> {
    if ip_packet.dst_addr() != gateway_ip {
        return None;
    }

    if ip_packet.next_header() != IpProtocol::Udp {
        return None;
    }

    let udp_packet = UdpPacket::new_checked(ip_packet.payload()).ok()?;

    if udp_packet.dst_port() != 53 {
        return None;
    }

    Some(DnsQueryInfo {
        guest_mac,
        guest_ip: ip_packet.src_addr(),
        guest_port: udp_packet.src_port(),
        query_bytes: udp_packet.payload().to_vec(),
    })
}

impl GatewayStack {
    /// Handle a frame received from the guest.
    ///
    /// Queues any response frames to `outbound` instead of blocking,
    /// ensuring the main loop can continue draining incoming frames.
    pub(super) async fn handle_guest_frame(
        &mut self,
        frame: Vec<u8>,
        _outbound: &mut OutboundFrameBuffer,
    ) {
        if parse_ipv4_frame(&frame).is_none() {
            // Non-IPv4 (e.g. ARP). With any_ip enabled, smoltcp responds to
            // ARP for ANY IP. Only feed ARP requests targeting the gateway IP.
            if is_arp_request_for(&frame, self.config.gateway_ip) {
                self.device.queue_rx_frame(frame);
            }
            return;
        }

        match classify_frame(&frame, &self.config) {
            FrameClassification::DnsQuery(query_info) => {
                self.dns.dispatch_query(query_info);
            }
            FrameClassification::ExternalTcp(tcp_info) => {
                self.handle_external_tcp(frame, tcp_info);
            }
            FrameClassification::ExternalOther => {
                self.nat.process_frame(&frame).await;
            }
            FrameClassification::ForGateway => {
                // Feed gateway-destined or broadcast frames to smoltcp (ARP, ICMP
                // to gateway, DHCP). With any_ip enabled, smoltcp would otherwise
                // consume unicast frames meant for other VMs on the same subnet.
                self.device.queue_rx_frame(frame);
            }
            FrameClassification::Ignored => {}
        }
    }

    fn handle_external_tcp(&mut self, frame: Vec<u8>, info: ExternalTcpInfo) {
        if info.is_syn {
            let mut socket = new_smoltcp_tcp_socket();

            let endpoint = IpListenEndpoint {
                addr: Some(IpAddress::Ipv4(*info.remote_dst.ip())),
                port: info.remote_dst.port(),
            };

            if socket.listen(endpoint).is_err() {
                tracing::debug!("TCP manager: failed to listen on {}", info.remote_dst);
                return;
            }

            match self.tcp_manager.initiate(
                socket,
                info.remote_dst,
                info.guest_src,
                &mut self.sockets,
            ) {
                InitiateResult::Created(handle) => {
                    tracing::debug!(
                        dst = %info.remote_dst,
                        handle = ?handle,
                        "TCP manager: new outbound connection via smoltcp"
                    );
                }
                InitiateResult::DuplicateFlow => {
                    // Existing smoltcp socket needs the retransmitted SYN to
                    // complete its handshake — fall through to queue_rx_frame.
                }
                InitiateResult::RejectedLimit => {
                    tracing::warn!(
                        dst = %info.remote_dst,
                        "TCP manager: connection limit reached, dropping SYN"
                    );
                    return;
                }
            }
        }

        // Feed frame to smoltcp (SYN or data/ACK/FIN/RST for existing connections)
        self.device.queue_rx_frame(frame);
    }
}

/// Check if a raw Ethernet frame is an ARP request targeting `gateway_ip`.
///
/// With smoltcp's `any_ip` mode, it responds to ARP for ANY IP address.
/// We only forward ARP requests for the gateway to smoltcp so that
/// VM-to-VM ARP resolves correctly.
fn is_arp_request_for(frame: &[u8], gateway_ip: Ipv4Addr) -> bool {
    let Ok(eth) = EthernetFrame::new_checked(frame) else {
        return false;
    };
    if eth.ethertype() != EthernetProtocol::Arp {
        return false;
    }
    let Ok(arp) = ArpPacket::new_checked(eth.payload()) else {
        return false;
    };
    if arp.operation() != ArpOperation::Request {
        return false;
    }
    let addr = arp.target_protocol_addr();
    if addr.len() != 4 {
        return false;
    }
    Ipv4Addr::new(addr[0], addr[1], addr[2], addr[3]) == gateway_ip
}

/// Check if the Ethernet destination MAC indicates the frame is for the gateway.
fn is_mac_for_gateway(dst_mac: EthernetAddress, gateway_mac: EthernetAddress) -> bool {
    dst_mac == gateway_mac || dst_mac.is_broadcast() || dst_mac.is_multicast()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mac_for_gateway_unicast_to_gateway() {
        let gateway = EthernetAddress([0x52, 0x54, 0x00, 0, 0, 1]);
        assert!(is_mac_for_gateway(gateway, gateway));
    }

    #[test]
    fn mac_for_gateway_unicast_to_other_vm_rejected() {
        let gateway = EthernetAddress([0x52, 0x54, 0x00, 0, 0, 1]);
        let guest = EthernetAddress([0x52, 0x54, 0x00, 0, 0, 2]);
        assert!(!is_mac_for_gateway(guest, gateway));
    }
}
