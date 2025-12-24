use smoltcp::wire::{
    EthernetFrame, EthernetProtocol, IpProtocol, Ipv4Packet, TcpPacket, UdpPacket,
};
use std::collections::HashMap;
use std::io;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;

/// Tracks NAT connections and handles packet forwarding.
pub struct NatTable {
    tcp_connections: HashMap<TcpKey, TcpNatEntry>,
    udp_bindings: HashMap<UdpKey, UdpNatEntry>,
    /// Our gateway IP
    gateway_ip: Ipv4Addr,
    /// Channel to send frames back to guest
    tx_to_guest: mpsc::Sender<Vec<u8>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct TcpKey {
    src: SocketAddrV4,
    dst: SocketAddrV4,
}

struct TcpNatEntry {
    host_stream: TcpStream,
    state: TcpNatState,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum TcpNatState {
    Connecting,
    Established,
    Closing,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct UdpKey {
    src: SocketAddrV4,
}

struct UdpNatEntry {
    socket: UdpSocket,
}

impl NatTable {
    pub fn new(gateway_ip: Ipv4Addr, tx_to_guest: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            tcp_connections: HashMap::new(),
            udp_bindings: HashMap::new(),
            gateway_ip,
            tx_to_guest,
        }
    }

    /// Process an ethernet frame from the guest.
    /// Returns true if the frame was handled (NAT'd), false if it should be
    /// processed by smoltcp (e.g., ARP, ICMP to gateway, DHCP).
    pub async fn process_frame(&mut self, frame: &[u8]) -> bool {
        let Ok(eth_frame) = EthernetFrame::new_checked(frame) else {
            return false;
        };

        // Only handle IPv4 for now
        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return false;
        }

        let Ok(ip_packet) = Ipv4Packet::new_checked(eth_frame.payload()) else {
            return false;
        };

        let dst_ip: Ipv4Addr = ip_packet.dst_addr().into();

        // If destination is our gateway IP, let smoltcp handle it (ICMP, DHCP, etc.)
        if dst_ip == self.gateway_ip {
            return false;
        }

        // External destination - handle NAT
        match ip_packet.next_header() {
            IpProtocol::Tcp => self.handle_tcp(&ip_packet).await,
            IpProtocol::Udp => self.handle_udp(&ip_packet).await,
            _ => false,
        }
    }

    async fn handle_tcp(&mut self, ip_packet: &Ipv4Packet<&[u8]>) -> bool {
        let Ok(tcp_packet) = TcpPacket::new_checked(ip_packet.payload()) else {
            return false;
        };

        let src = SocketAddrV4::new(ip_packet.src_addr().into(), tcp_packet.src_port());
        let dst = SocketAddrV4::new(ip_packet.dst_addr().into(), tcp_packet.dst_port());
        let key = TcpKey { src, dst };

        // Check for SYN (new connection)
        if tcp_packet.syn() && !tcp_packet.ack() {
            // New connection request
            if !self.tcp_connections.contains_key(&key) {
                match TcpStream::connect(SocketAddr::V4(dst)).await {
                    Ok(stream) => {
                        tracing::debug!("NAT: TCP connection {} -> {}", src, dst);
                        self.tcp_connections.insert(
                            key,
                            TcpNatEntry {
                                host_stream: stream,
                                state: TcpNatState::Connecting,
                            },
                        );
                        // TODO: Send SYN-ACK back to guest
                    }
                    Err(e) => {
                        tracing::warn!("NAT: TCP connect to {} failed: {}", dst, e);
                        // TODO: Send RST back to guest
                    }
                }
            }
            return true;
        }

        // TODO: Handle data packets, ACKs, FINs
        // This requires maintaining TCP state and sequence numbers

        true
    }

    async fn handle_udp(&mut self, ip_packet: &Ipv4Packet<&[u8]>) -> bool {
        let Ok(udp_packet) = UdpPacket::new_checked(ip_packet.payload()) else {
            return false;
        };

        let src = SocketAddrV4::new(ip_packet.src_addr().into(), udp_packet.src_port());
        let dst = SocketAddrV4::new(ip_packet.dst_addr().into(), udp_packet.dst_port());
        let key = UdpKey { src };

        // Get or create UDP socket for this source
        let socket = match self.udp_bindings.get(&key) {
            Some(entry) => &entry.socket,
            None => {
                let socket = match UdpSocket::bind("0.0.0.0:0").await {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("NAT: UDP bind failed: {}", e);
                        return false;
                    }
                };
                self.udp_bindings.insert(key, UdpNatEntry { socket });
                &self.udp_bindings.get(&key).unwrap().socket
            }
        };

        // Forward the UDP packet
        let payload = udp_packet.payload();
        if let Err(e) = socket.send_to(payload, SocketAddr::V4(dst)).await {
            tracing::warn!("NAT: UDP send to {} failed: {}", dst, e);
        } else {
            tracing::debug!("NAT: UDP {} -> {} ({} bytes)", src, dst, payload.len());
        }

        // TODO: Set up receive task to forward responses back to guest

        true
    }
}
