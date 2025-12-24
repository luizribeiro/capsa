//! NAT connection tracking and packet forwarding.
//!
//! This module implements userspace NAT for TCP and UDP connections from the guest
//! to external hosts. It intercepts packets destined for external IPs and forwards
//! them through host sockets, then crafts response packets back to the guest.

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, EthernetRepr, IpProtocol, Ipv4Address,
    Ipv4Packet, Ipv4Repr, UdpPacket, UdpRepr,
};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Idle timeout for UDP NAT entries. After this duration without activity,
/// entries are cleaned up and their background tasks are canceled.
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Channel for sending frames back to the guest.
pub type FrameSender = mpsc::Sender<Vec<u8>>;
pub type FrameReceiver = mpsc::Receiver<Vec<u8>>;

/// Creates a channel pair for NAT response frames.
pub fn frame_channel(buffer: usize) -> (FrameSender, FrameReceiver) {
    mpsc::channel(buffer)
}

/// NAT connection tracker handling UDP and TCP forwarding.
pub struct NatTable {
    /// UDP bindings: guest source -> host socket and metadata
    udp_bindings: HashMap<UdpKey, UdpNatEntry>,
    /// Gateway IP (our IP on the virtual network)
    gateway_ip: Ipv4Addr,
    /// Gateway MAC address
    gateway_mac: EthernetAddress,
    /// Channel to send response frames back to guest
    tx_to_guest: FrameSender,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct UdpKey {
    guest_addr: SocketAddrV4,
}

struct UdpNatEntry {
    socket: Arc<UdpSocket>,
    task_handle: JoinHandle<()>,
    last_activity: Instant,
}

impl NatTable {
    pub fn new(gateway_ip: Ipv4Addr, gateway_mac: [u8; 6], tx_to_guest: FrameSender) -> Self {
        Self {
            udp_bindings: HashMap::new(),
            gateway_ip,
            gateway_mac: EthernetAddress(gateway_mac),
            tx_to_guest,
        }
    }

    /// Process an ethernet frame from the guest.
    ///
    /// Returns true if the frame was handled (NAT'd), false if it should be
    /// processed by smoltcp (e.g., ARP, ICMP to gateway, DHCP).
    pub async fn process_frame(&mut self, frame: &[u8]) -> bool {
        let Ok(eth_frame) = EthernetFrame::new_checked(frame) else {
            return false;
        };

        // Only handle IPv4
        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return false;
        }

        let Ok(ip_packet) = Ipv4Packet::new_checked(eth_frame.payload()) else {
            return false;
        };

        let dst_ip: Ipv4Addr = ip_packet.dst_addr().into();

        // If destination is our gateway IP, let smoltcp handle it
        if dst_ip == self.gateway_ip {
            return false;
        }

        // External destination - handle NAT
        let guest_mac = eth_frame.src_addr();
        match ip_packet.next_header() {
            IpProtocol::Udp => self.handle_udp(guest_mac, &ip_packet).await,
            IpProtocol::Tcp => {
                // TCP NAT not yet implemented
                tracing::debug!("NAT: TCP to external host not implemented yet");
                false
            }
            _ => false,
        }
    }

    async fn handle_udp(
        &mut self,
        guest_mac: EthernetAddress,
        ip_packet: &Ipv4Packet<&[u8]>,
    ) -> bool {
        let Ok(udp_packet) = UdpPacket::new_checked(ip_packet.payload()) else {
            return false;
        };

        let src_ip: Ipv4Addr = ip_packet.src_addr().into();
        let dst_ip: Ipv4Addr = ip_packet.dst_addr().into();
        let src = SocketAddrV4::new(src_ip, udp_packet.src_port());
        let dst = SocketAddrV4::new(dst_ip, udp_packet.dst_port());
        let key = UdpKey { guest_addr: src };

        // Get or create UDP socket for this guest source
        let socket = if let Some(entry) = self.udp_bindings.get_mut(&key) {
            entry.last_activity = Instant::now();
            entry.socket.clone()
        } else {
            // Create new socket and spawn receive task
            let socket = match UdpSocket::bind("0.0.0.0:0").await {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::warn!("NAT: UDP bind failed: {}", e);
                    return false;
                }
            };

            // Spawn task to receive responses and forward to guest
            let socket_clone = socket.clone();
            let tx = self.tx_to_guest.clone();
            let guest_addr = src;
            let gateway_ip = self.gateway_ip;
            let gateway_mac = self.gateway_mac;

            let task_handle = tokio::spawn(async move {
                // 4KB buffer is sufficient for DNS, DHCP, and most UDP protocols
                let mut buf = vec![0u8; 4096];
                loop {
                    match socket_clone.recv_from(&mut buf).await {
                        Ok((len, remote_addr)) => {
                            let remote = match remote_addr {
                                SocketAddr::V4(v4) => v4,
                                SocketAddr::V6(_) => continue,
                            };

                            // Craft response frame
                            if let Some(frame) = craft_udp_response(
                                &buf[..len],
                                remote,
                                guest_addr,
                                gateway_ip,
                                gateway_mac,
                                guest_mac,
                            ) {
                                if tx.send(frame).await.is_err() {
                                    tracing::debug!(
                                        "NAT: Response channel closed for {}, terminating",
                                        guest_addr
                                    );
                                    break;
                                }
                            }
                        }
                        Err(e) => {
                            tracing::debug!("NAT: UDP recv error for {}: {}", guest_addr, e);
                            break;
                        }
                    }
                }
            });

            self.udp_bindings.insert(
                key,
                UdpNatEntry {
                    socket: socket.clone(),
                    task_handle,
                    last_activity: Instant::now(),
                },
            );

            socket
        };

        // Forward the UDP packet
        let payload = udp_packet.payload();
        match socket.send_to(payload, SocketAddr::V4(dst)).await {
            Ok(_) => {
                tracing::debug!("NAT: UDP {} -> {} ({} bytes)", src, dst, payload.len());
                true
            }
            Err(e) => {
                tracing::warn!("NAT: UDP send to {} failed: {}", dst, e);
                false
            }
        }
    }

    /// Clean up stale NAT entries (called periodically).
    /// Removes entries that have been idle for more than UDP_IDLE_TIMEOUT
    /// and aborts their background receive tasks.
    pub fn cleanup(&mut self) {
        let now = Instant::now();
        self.udp_bindings.retain(|key, entry| {
            let idle_duration = now.duration_since(entry.last_activity);
            if idle_duration > UDP_IDLE_TIMEOUT {
                tracing::debug!(
                    "NAT: Cleaning up idle UDP entry for {} (idle for {:?})",
                    key.guest_addr,
                    idle_duration
                );
                entry.task_handle.abort();
                false
            } else {
                true
            }
        });
    }
}

/// Craft a UDP response ethernet frame to send back to guest.
fn craft_udp_response(
    payload: &[u8],
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    _gateway_ip: Ipv4Addr,
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    // Calculate sizes
    let udp_len = 8 + payload.len(); // UDP header + payload
    let ip_len = 20 + udp_len; // IP header + UDP
    let total_len = 14 + ip_len; // Ethernet header + IP

    let mut frame = vec![0u8; total_len];

    // Build ethernet header
    let eth_repr = EthernetRepr {
        src_addr: gateway_mac, // Response comes from gateway MAC
        dst_addr: guest_mac,
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
    eth_repr.emit(&mut eth_frame);

    // Build IP header
    let ip_repr = Ipv4Repr {
        src_addr: Ipv4Address::from(*src_addr.ip()),
        dst_addr: Ipv4Address::from(*dst_addr.ip()),
        next_header: IpProtocol::Udp,
        payload_len: udp_len,
        hop_limit: 64,
    };

    let mut ip_packet = Ipv4Packet::new_unchecked(&mut frame[14..]);
    let checksum_caps = ChecksumCapabilities::default();
    ip_repr.emit(&mut ip_packet, &checksum_caps);

    // Build UDP header
    let udp_repr = UdpRepr {
        src_port: src_addr.port(),
        dst_port: dst_addr.port(),
    };

    let mut udp_packet = UdpPacket::new_unchecked(&mut frame[14 + 20..]);
    udp_repr.emit(
        &mut udp_packet,
        &ip_repr.src_addr.into(),
        &ip_repr.dst_addr.into(),
        payload.len(),
        |buf| buf.copy_from_slice(payload),
        &checksum_caps,
    );

    Some(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_craft_udp_response() {
        let payload = b"hello";
        let src = SocketAddrV4::new(Ipv4Addr::new(8, 8, 8, 8), 53);
        let dst = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 15), 12345);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        let frame = craft_udp_response(
            payload,
            src,
            dst,
            Ipv4Addr::new(10, 0, 2, 2),
            gateway_mac,
            guest_mac,
        );

        assert!(frame.is_some());
        let frame = frame.unwrap();

        // Verify ethernet header
        let eth = EthernetFrame::new_checked(&frame).unwrap();
        assert_eq!(eth.src_addr(), gateway_mac);
        assert_eq!(eth.dst_addr(), guest_mac);
        assert_eq!(eth.ethertype(), EthernetProtocol::Ipv4);

        // Verify IP header
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert_eq!(Ipv4Addr::from(ip.src_addr()), *src.ip());
        assert_eq!(Ipv4Addr::from(ip.dst_addr()), *dst.ip());

        // Verify UDP header
        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        assert_eq!(udp.src_port(), src.port());
        assert_eq!(udp.dst_port(), dst.port());
        assert_eq!(udp.payload(), payload);
    }
}
