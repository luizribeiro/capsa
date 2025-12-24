//! NAT connection tracking and packet forwarding.
//!
//! This module implements userspace NAT for TCP and UDP connections from the guest
//! to external hosts. It intercepts packets destined for external IPs and forwards
//! them through host sockets, then crafts response packets back to the guest.

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, EthernetRepr, IpProtocol, Ipv4Packet,
    Ipv4Repr, TcpPacket, TcpRepr, TcpSeqNumber, UdpPacket, UdpRepr,
};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Idle timeout for UDP NAT entries.
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Idle timeout for TCP NAT entries.
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

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
    /// TCP connections: (guest_addr, remote_addr) -> connection state
    tcp_connections: HashMap<TcpKey, TcpNatEntry>,
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

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct TcpKey {
    guest_addr: SocketAddrV4,
    remote_addr: SocketAddrV4,
}

/// TCP connection state for NAT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TcpState {
    /// SYN received from guest, connecting to remote
    SynReceived,
    /// Connection established, data can flow
    Established,
    /// FIN received from guest, waiting for remote close
    FinWait,
    /// Connection closed
    Closed,
}

struct TcpNatEntry {
    state: TcpState,
    /// Guest's MAC address for crafting responses
    guest_mac: EthernetAddress,
    /// Our sequence number for responses to guest
    our_seq: u32,
    /// Next expected sequence from guest
    guest_next_seq: u32,
    /// Handle to the bidirectional forwarding task
    task_handle: JoinHandle<()>,
    /// Channel to send data to the host socket
    data_tx: mpsc::Sender<Vec<u8>>,
    /// Last activity time for cleanup
    last_activity: Instant,
}

impl NatTable {
    pub fn new(gateway_ip: Ipv4Addr, gateway_mac: [u8; 6], tx_to_guest: FrameSender) -> Self {
        Self {
            udp_bindings: HashMap::new(),
            tcp_connections: HashMap::new(),
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

        let dst_ip: Ipv4Addr = ip_packet.dst_addr();

        // If destination is our gateway IP, let smoltcp handle it
        if dst_ip == self.gateway_ip {
            return false;
        }

        // External destination - handle NAT
        let guest_mac = eth_frame.src_addr();
        match ip_packet.next_header() {
            IpProtocol::Udp => self.handle_udp(guest_mac, &ip_packet).await,
            IpProtocol::Tcp => self.handle_tcp(guest_mac, &ip_packet).await,
            _ => false,
        }
    }

    async fn handle_tcp(
        &mut self,
        guest_mac: EthernetAddress,
        ip_packet: &Ipv4Packet<&[u8]>,
    ) -> bool {
        let Ok(tcp_packet) = TcpPacket::new_checked(ip_packet.payload()) else {
            return false;
        };

        let src_ip: Ipv4Addr = ip_packet.src_addr();
        let dst_ip: Ipv4Addr = ip_packet.dst_addr();
        let guest_addr = SocketAddrV4::new(src_ip, tcp_packet.src_port());
        let remote_addr = SocketAddrV4::new(dst_ip, tcp_packet.dst_port());
        let key = TcpKey {
            guest_addr,
            remote_addr,
        };

        // Handle based on TCP flags
        let syn = tcp_packet.syn();
        let ack = tcp_packet.ack();
        let fin = tcp_packet.fin();
        let rst = tcp_packet.rst();

        if rst {
            // RST received - close connection
            if let Some(entry) = self.tcp_connections.remove(&key) {
                tracing::debug!("NAT: TCP RST from guest {}", guest_addr);
                entry.task_handle.abort();
            }
            return true;
        }

        if syn && !ack {
            // New connection (SYN without ACK)
            return self
                .handle_tcp_syn(key, guest_mac, tcp_packet.seq_number().0 as u32)
                .await;
        }

        // Existing connection
        if let Some(entry) = self.tcp_connections.get_mut(&key) {
            entry.last_activity = Instant::now();

            if fin {
                // FIN received - initiate close
                return self.handle_tcp_fin(&key, &tcp_packet).await;
            }

            // Handle data or transition state on ACK
            let payload = tcp_packet.payload();

            if entry.state == TcpState::SynReceived && ack {
                // Third leg of handshake - transition to Established
                entry.state = TcpState::Established;
                tracing::debug!(
                    "NAT: TCP {} -> {} connection established",
                    key.guest_addr,
                    key.remote_addr
                );
            }

            if entry.state == TcpState::Established {
                if !payload.is_empty() {
                    return self.handle_tcp_data(&key, &tcp_packet).await;
                }
                // Pure ACK - just acknowledge
                return true;
            }
        }

        false
    }

    async fn handle_tcp_syn(
        &mut self,
        key: TcpKey,
        guest_mac: EthernetAddress,
        guest_isn: u32,
    ) -> bool {
        if self.tcp_connections.contains_key(&key) {
            // Connection already exists, ignore duplicate SYN
            return true;
        }

        tracing::debug!(
            "NAT: TCP SYN {} -> {}, connecting...",
            key.guest_addr,
            key.remote_addr
        );

        // Try to connect to the remote host
        let stream = match TcpStream::connect(SocketAddr::V4(key.remote_addr)).await {
            Ok(s) => s,
            Err(e) => {
                tracing::debug!("NAT: TCP connect to {} failed: {}", key.remote_addr, e);
                // Send RST back to guest
                if let Some(frame) = craft_tcp_rst(
                    key.remote_addr,
                    key.guest_addr,
                    0,
                    guest_isn.wrapping_add(1),
                    self.gateway_mac,
                    guest_mac,
                ) {
                    let _ = self.tx_to_guest.send(frame).await;
                }
                return true;
            }
        };

        // Generate our initial sequence number
        let our_isn: u32 = rand::random();

        // Create channel for sending data to the socket
        let (data_tx, mut data_rx) = mpsc::channel::<Vec<u8>>(64);

        // Spawn bidirectional forwarding task
        let tx_to_guest = self.tx_to_guest.clone();
        let gateway_mac = self.gateway_mac;
        let guest_addr = key.guest_addr;
        let remote_addr = key.remote_addr;
        let mut our_seq = our_isn.wrapping_add(1); // After SYN-ACK
        let mut guest_ack = guest_isn.wrapping_add(1);

        let task_handle = tokio::spawn(async move {
            let (mut read_half, mut write_half) = stream.into_split();
            let mut buf = vec![0u8; 4096];

            loop {
                tokio::select! {
                    // Data from guest to send to remote
                    Some(data) = data_rx.recv() => {
                        if write_half.write_all(&data).await.is_err() {
                            break;
                        }
                        guest_ack = guest_ack.wrapping_add(data.len() as u32);
                    }

                    // Data from remote to send to guest
                    result = read_half.read(&mut buf) => {
                        match result {
                            Ok(0) => {
                                // Remote closed - send FIN to guest
                                if let Some(frame) = craft_tcp_fin(
                                    remote_addr,
                                    guest_addr,
                                    our_seq,
                                    guest_ack,
                                    gateway_mac,
                                    guest_mac,
                                ) {
                                    let _ = tx_to_guest.send(frame).await;
                                }
                                break;
                            }
                            Ok(n) => {
                                // Send data to guest
                                if let Some(frame) = craft_tcp_data(
                                    remote_addr,
                                    guest_addr,
                                    our_seq,
                                    guest_ack,
                                    &buf[..n],
                                    gateway_mac,
                                    guest_mac,
                                ) {
                                    if tx_to_guest.send(frame).await.is_err() {
                                        break;
                                    }
                                    our_seq = our_seq.wrapping_add(n as u32);
                                }
                            }
                            Err(_) => break,
                        }
                    }
                }
            }
        });

        // Send SYN-ACK to guest
        if let Some(frame) = craft_tcp_syn_ack(
            key.remote_addr,
            key.guest_addr,
            our_isn,
            guest_isn.wrapping_add(1),
            self.gateway_mac,
            guest_mac,
        ) {
            let _ = self.tx_to_guest.send(frame).await;
        }

        self.tcp_connections.insert(
            key,
            TcpNatEntry {
                state: TcpState::SynReceived,
                guest_mac,
                our_seq: our_isn.wrapping_add(1),
                guest_next_seq: guest_isn.wrapping_add(1),
                task_handle,
                data_tx,
                last_activity: Instant::now(),
            },
        );

        true
    }

    async fn handle_tcp_data(&mut self, key: &TcpKey, tcp_packet: &TcpPacket<&[u8]>) -> bool {
        let Some(entry) = self.tcp_connections.get_mut(key) else {
            return false;
        };

        let payload = tcp_packet.payload();
        if payload.is_empty() {
            return true;
        }

        // Send data to the forwarding task
        let data = payload.to_vec();
        if entry.data_tx.send(data).await.is_err() {
            // Task died, remove connection
            if let Some(entry) = self.tcp_connections.remove(key) {
                entry.task_handle.abort();
            }
            return false;
        }

        // Update expected sequence
        entry.guest_next_seq = entry.guest_next_seq.wrapping_add(payload.len() as u32);

        // Send ACK back to guest
        if let Some(frame) = craft_tcp_ack(
            key.remote_addr,
            key.guest_addr,
            entry.our_seq,
            entry.guest_next_seq,
            self.gateway_mac,
            entry.guest_mac,
        ) {
            let _ = self.tx_to_guest.send(frame).await;
        }

        true
    }

    async fn handle_tcp_fin(&mut self, key: &TcpKey, tcp_packet: &TcpPacket<&[u8]>) -> bool {
        let Some(entry) = self.tcp_connections.get_mut(key) else {
            return false;
        };

        entry.state = TcpState::FinWait;

        // Send ACK for FIN
        let fin_seq = tcp_packet.seq_number().0 as u32;
        if let Some(frame) = craft_tcp_ack(
            key.remote_addr,
            key.guest_addr,
            entry.our_seq,
            fin_seq.wrapping_add(1),
            self.gateway_mac,
            entry.guest_mac,
        ) {
            let _ = self.tx_to_guest.send(frame).await;
        }

        // Close the connection
        if let Some(entry) = self.tcp_connections.remove(key) {
            entry.task_handle.abort();
        }

        true
    }

    async fn handle_udp(
        &mut self,
        guest_mac: EthernetAddress,
        ip_packet: &Ipv4Packet<&[u8]>,
    ) -> bool {
        let Ok(udp_packet) = UdpPacket::new_checked(ip_packet.payload()) else {
            return false;
        };

        let src_ip: Ipv4Addr = ip_packet.src_addr();
        let dst_ip: Ipv4Addr = ip_packet.dst_addr();
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
                            ) && tx.send(frame).await.is_err()
                            {
                                tracing::debug!(
                                    "NAT: Response channel closed for {}, terminating",
                                    guest_addr
                                );
                                break;
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
    /// Removes entries that have been idle for more than their respective timeouts
    /// and aborts their background tasks.
    pub fn cleanup(&mut self) {
        let now = Instant::now();

        // Cleanup UDP entries
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

        // Cleanup TCP connections
        self.tcp_connections.retain(|key, entry| {
            let idle_duration = now.duration_since(entry.last_activity);
            if idle_duration > TCP_IDLE_TIMEOUT || entry.state == TcpState::Closed {
                tracing::debug!(
                    "NAT: Cleaning up TCP connection {} -> {} (idle for {:?})",
                    key.guest_addr,
                    key.remote_addr,
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
#[allow(clippy::useless_conversion)] // Ipv4Address -> IpAddress is needed for emit()
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
        src_addr: *src_addr.ip(),
        dst_addr: *dst_addr.ip(),
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

/// Craft a TCP SYN-ACK frame to send back to guest.
fn craft_tcp_syn_ack(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    seq_num: u32,
    ack_num: u32,
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    craft_tcp_frame(
        src_addr,
        dst_addr,
        seq_num,
        ack_num,
        TcpControl::Syn,
        &[],
        gateway_mac,
        guest_mac,
    )
}

/// Craft a TCP ACK frame to send back to guest.
fn craft_tcp_ack(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    seq_num: u32,
    ack_num: u32,
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    craft_tcp_frame(
        src_addr,
        dst_addr,
        seq_num,
        ack_num,
        TcpControl::None,
        &[],
        gateway_mac,
        guest_mac,
    )
}

/// Craft a TCP data frame to send back to guest.
fn craft_tcp_data(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    seq_num: u32,
    ack_num: u32,
    payload: &[u8],
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    craft_tcp_frame(
        src_addr,
        dst_addr,
        seq_num,
        ack_num,
        TcpControl::None,
        payload,
        gateway_mac,
        guest_mac,
    )
}

/// Craft a TCP FIN frame to send back to guest.
fn craft_tcp_fin(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    seq_num: u32,
    ack_num: u32,
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    craft_tcp_frame(
        src_addr,
        dst_addr,
        seq_num,
        ack_num,
        TcpControl::Fin,
        &[],
        gateway_mac,
        guest_mac,
    )
}

/// Craft a TCP RST frame to send back to guest.
fn craft_tcp_rst(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    seq_num: u32,
    ack_num: u32,
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    craft_tcp_frame(
        src_addr,
        dst_addr,
        seq_num,
        ack_num,
        TcpControl::Rst,
        &[],
        gateway_mac,
        guest_mac,
    )
}

/// TCP control flags for crafting responses.
#[derive(Clone, Copy)]
enum TcpControl {
    None,
    Syn,
    Fin,
    Rst,
}

/// Common function to craft TCP frames.
#[allow(clippy::useless_conversion)] // Ipv4Address -> IpAddress is needed for emit()
#[allow(clippy::too_many_arguments)]
fn craft_tcp_frame(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    seq_num: u32,
    ack_num: u32,
    control: TcpControl,
    payload: &[u8],
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    // TCP header is 20 bytes minimum
    let tcp_len = 20 + payload.len();
    let ip_len = 20 + tcp_len;
    let total_len = 14 + ip_len;

    let mut frame = vec![0u8; total_len];

    // Build ethernet header
    let eth_repr = EthernetRepr {
        src_addr: gateway_mac,
        dst_addr: guest_mac,
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
    eth_repr.emit(&mut eth_frame);

    // Build IP header
    let ip_repr = Ipv4Repr {
        src_addr: *src_addr.ip(),
        dst_addr: *dst_addr.ip(),
        next_header: IpProtocol::Tcp,
        payload_len: tcp_len,
        hop_limit: 64,
    };

    let mut ip_packet = Ipv4Packet::new_unchecked(&mut frame[14..]);
    let checksum_caps = ChecksumCapabilities::default();
    ip_repr.emit(&mut ip_packet, &checksum_caps);

    // Build TCP header
    let tcp_repr = TcpRepr {
        src_port: src_addr.port(),
        dst_port: dst_addr.port(),
        seq_number: TcpSeqNumber(seq_num as i32),
        ack_number: Some(TcpSeqNumber(ack_num as i32)),
        window_len: 65535,
        window_scale: None,
        control: match control {
            TcpControl::None => smoltcp::wire::TcpControl::None,
            TcpControl::Syn => smoltcp::wire::TcpControl::Syn,
            TcpControl::Fin => smoltcp::wire::TcpControl::Fin,
            TcpControl::Rst => smoltcp::wire::TcpControl::Rst,
        },
        max_seg_size: None,
        sack_permitted: false,
        sack_ranges: [None, None, None],
        timestamp: None,
        payload,
    };

    let mut tcp_packet = TcpPacket::new_unchecked(&mut frame[14 + 20..]);
    tcp_repr.emit(
        &mut tcp_packet,
        &ip_repr.src_addr.into(),
        &ip_repr.dst_addr.into(),
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
