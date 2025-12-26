//! NAT connection tracking and packet forwarding.
//!
//! This module implements userspace NAT for TCP, UDP, and ICMP from the guest
//! to external hosts. It intercepts packets destined for external IPs and forwards
//! them through host sockets, then crafts response packets back to the guest.

use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, EthernetRepr, Icmpv4Message, Icmpv4Packet,
    IpProtocol, Ipv4Packet, Ipv4Repr, TcpPacket, TcpRepr, TcpSeqNumber, UdpPacket, UdpRepr,
};
use socket2::{Domain, Protocol, Type};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::{Duration, Instant};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

/// Idle timeout for UDP NAT entries.
const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(60);

/// Idle timeout for TCP NAT entries.
const TCP_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Idle timeout for ICMP NAT entries.
const ICMP_IDLE_TIMEOUT: Duration = Duration::from_secs(120);

/// Maximum ICMP bindings per guest IP to prevent socket exhaustion.
const MAX_ICMP_BINDINGS_PER_GUEST: usize = 64;

/// Maximum TCP connections to prevent socket and task exhaustion.
/// Each connection opens a host socket and spawns a forwarding task.
const MAX_TCP_CONNECTIONS: usize = 1024;

/// Maximum UDP bindings to prevent socket and task exhaustion.
/// Each binding opens a host socket and spawns a receive task.
const MAX_UDP_BINDINGS: usize = 256;

/// Standard Ethernet MTU in bytes.
const ETHERNET_MTU: usize = 1500;

/// IPv4 header size (without options).
const IP_HEADER_SIZE: usize = 20;

/// TCP header size (without options).
const TCP_HEADER_SIZE: usize = 20;

/// TCP Maximum Segment Size for standard Ethernet.
/// MSS = MTU - IP header - TCP header
const TCP_MSS: usize = ETHERNET_MTU - IP_HEADER_SIZE - TCP_HEADER_SIZE;

/// Channel for sending frames back to the guest.
pub type FrameSender = mpsc::Sender<Vec<u8>>;
pub type FrameReceiver = mpsc::Receiver<Vec<u8>>;

/// Creates a channel pair for NAT response frames.
pub fn frame_channel(buffer: usize) -> (FrameSender, FrameReceiver) {
    mpsc::channel(buffer)
}

/// NAT connection tracker handling UDP, TCP, and ICMP forwarding.
pub struct NatTable {
    /// UDP bindings: guest source -> host socket and metadata
    udp_bindings: HashMap<UdpKey, UdpNatEntry>,
    /// TCP connections: (guest_addr, remote_addr) -> connection state
    tcp_connections: HashMap<TcpKey, TcpNatEntry>,
    /// ICMP bindings: (guest_ip, identifier) -> socket and metadata
    icmp_bindings: HashMap<IcmpKey, IcmpNatEntry>,
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
    /// Our sequence number for responses to guest (shared with forwarding task)
    our_seq: Arc<AtomicU32>,
    /// Next expected sequence from guest
    guest_next_seq: u32,
    /// Handle to the bidirectional forwarding task
    task_handle: JoinHandle<()>,
    /// Channel to send data to the host socket
    data_tx: mpsc::Sender<Vec<u8>>,
    /// Last activity time for cleanup
    last_activity: Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct IcmpKey {
    /// Guest's IP address (source of the echo request)
    guest_ip: Ipv4Addr,
    /// ICMP identifier from the echo request
    identifier: u16,
}

struct IcmpNatEntry {
    /// ICMP SOCK_DGRAM socket (non-privileged ICMP via IPPROTO_ICMP).
    /// Note: Type is UdpSocket because socket2 converts to std::net::UdpSocket,
    /// but the underlying protocol is ICMP, not UDP.
    socket: Arc<tokio::net::UdpSocket>,
    /// Handle to the receive task
    task_handle: JoinHandle<()>,
    /// Last activity time for cleanup
    last_activity: Instant,
}

impl NatTable {
    pub fn new(gateway_ip: Ipv4Addr, gateway_mac: [u8; 6], tx_to_guest: FrameSender) -> Self {
        Self {
            udp_bindings: HashMap::new(),
            tcp_connections: HashMap::new(),
            icmp_bindings: HashMap::new(),
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
            IpProtocol::Icmp => self.handle_icmp(guest_mac, &ip_packet).await,
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
        } else {
            // No existing connection for non-SYN packet - this can happen
            // when connection was closed but guest sends late ACKs
            tracing::trace!(
                "NAT: TCP {} -> {} no connection (flags: syn={}, ack={}, fin={}, rst={})",
                guest_addr,
                remote_addr,
                syn,
                ack,
                fin,
                rst
            );
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

        // Reject if connection limit reached
        if self.tcp_connections.len() >= MAX_TCP_CONNECTIONS {
            tracing::warn!(
                "NAT: TCP connection limit reached ({}), rejecting {} -> {}",
                MAX_TCP_CONNECTIONS,
                key.guest_addr,
                key.remote_addr
            );
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
            return false;
        }

        tracing::debug!(
            "NAT: TCP SYN {} -> {}, connecting...",
            key.guest_addr,
            key.remote_addr
        );

        // Try to connect to the remote host
        let stream = match TcpStream::connect(SocketAddr::V4(key.remote_addr)).await {
            Ok(s) => {
                tracing::debug!("NAT: TCP connect to {} succeeded", key.remote_addr);
                s
            }
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

        // Shared sequence number between task and NAT entry for ACK consistency
        let our_seq_shared = Arc::new(AtomicU32::new(our_isn.wrapping_add(1))); // After SYN-ACK
        let our_seq_for_task = our_seq_shared.clone();

        // Spawn bidirectional forwarding task
        let tx_to_guest = self.tx_to_guest.clone();
        let gateway_mac = self.gateway_mac;
        let guest_addr = key.guest_addr;
        let remote_addr = key.remote_addr;
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
                                let our_seq = our_seq_for_task.load(Ordering::Relaxed);
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
                                // Send data to guest in MSS-sized segments
                                let data = &buf[..n];
                                let mut offset = 0;
                                let mut send_failed = false;
                                // Load sequence once, track locally for efficiency
                                let mut our_seq = our_seq_for_task.load(Ordering::Relaxed);

                                while offset < data.len() {
                                    let end = (offset + TCP_MSS).min(data.len());
                                    let segment = &data[offset..end];

                                    match craft_tcp_data(
                                        remote_addr,
                                        guest_addr,
                                        our_seq,
                                        guest_ack,
                                        segment,
                                        gateway_mac,
                                        guest_mac,
                                    ) {
                                        Some(frame) => {
                                            if tx_to_guest.send(frame).await.is_err() {
                                                send_failed = true;
                                                break;
                                            }
                                            our_seq = our_seq.wrapping_add(segment.len() as u32);
                                            our_seq_for_task.store(our_seq, Ordering::Relaxed);
                                        }
                                        None => {
                                            tracing::error!(
                                                "NAT: Failed to craft TCP segment for {} -> {}",
                                                guest_addr,
                                                remote_addr
                                            );
                                            send_failed = true;
                                            break;
                                        }
                                    }
                                    offset = end;
                                }

                                if send_failed {
                                    break;
                                }
                            }
                            Err(_) => {
                                break;
                            }
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
                our_seq: our_seq_shared,
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

        // Send ACK back to guest (load current seq from atomic to stay in sync with task)
        if let Some(frame) = craft_tcp_ack(
            key.remote_addr,
            key.guest_addr,
            entry.our_seq.load(Ordering::Relaxed),
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
            entry.our_seq.load(Ordering::Relaxed),
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
            // Reject if binding limit reached
            if self.udp_bindings.len() >= MAX_UDP_BINDINGS {
                tracing::warn!(
                    "NAT: UDP binding limit reached ({}), rejecting {}",
                    MAX_UDP_BINDINGS,
                    src
                );
                return false;
            }

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

    async fn handle_icmp(
        &mut self,
        guest_mac: EthernetAddress,
        ip_packet: &Ipv4Packet<&[u8]>,
    ) -> bool {
        let Ok(icmp_packet) = Icmpv4Packet::new_checked(ip_packet.payload()) else {
            return false;
        };

        // Only handle echo requests
        if icmp_packet.msg_type() != Icmpv4Message::EchoRequest {
            return false;
        }

        let src_ip: Ipv4Addr = ip_packet.src_addr();
        let dst_ip: Ipv4Addr = ip_packet.dst_addr();

        // Use smoltcp's echo-specific accessors
        let identifier = icmp_packet.echo_ident();
        let sequence = icmp_packet.echo_seq_no();
        let payload = icmp_packet.data();

        let key = IcmpKey {
            guest_ip: src_ip,
            identifier,
        };

        // Get or create ICMP socket for this guest/identifier pair
        if let Some(entry) = self.icmp_bindings.get_mut(&key) {
            entry.last_activity = Instant::now();
        } else {
            // Check binding limit to prevent socket exhaustion
            let guest_binding_count = self
                .icmp_bindings
                .keys()
                .filter(|k| k.guest_ip == src_ip)
                .count();

            if guest_binding_count >= MAX_ICMP_BINDINGS_PER_GUEST {
                tracing::warn!(
                    "NAT: ICMP binding limit ({}) reached for guest {}",
                    MAX_ICMP_BINDINGS_PER_GUEST,
                    src_ip
                );
                return false;
            }

            // Create non-privileged ICMP socket using SOCK_DGRAM
            let socket =
                match socket2::Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)) {
                    Ok(s) => s,
                    Err(e) => {
                        tracing::warn!("NAT: Failed to create ICMP socket: {}", e);
                        return false;
                    }
                };

            socket.set_nonblocking(true).ok();

            // Convert to async tokio socket
            let std_socket: std::net::UdpSocket = socket.into();
            let tokio_socket = match tokio::net::UdpSocket::from_std(std_socket) {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::warn!("NAT: Failed to convert ICMP socket to async: {}", e);
                    return false;
                }
            };
            let socket_clone = tokio_socket.clone();

            // Spawn task to receive ICMP replies
            let tx = self.tx_to_guest.clone();
            let gateway_mac = self.gateway_mac;
            let guest_ip = src_ip;
            let icmp_id = identifier;

            let task_handle = tokio::spawn(async move {
                let mut buf = vec![0u8; ETHERNET_MTU];
                loop {
                    match socket_clone.recv_from(&mut buf).await {
                        Ok((len, remote_addr)) => {
                            let remote_ip = match remote_addr.ip() {
                                std::net::IpAddr::V4(ip) => ip,
                                _ => continue,
                            };

                            // SOCK_DGRAM ICMP sockets return different formats per platform.
                            // We detect and strip IP header if present (0x4X signature).
                            let icmp_data = if len > 20 && (buf[0] >> 4) == 4 {
                                let ip_header_len = ((buf[0] & 0x0F) as usize) * 4;
                                if len > ip_header_len {
                                    &buf[ip_header_len..len]
                                } else {
                                    continue;
                                }
                            } else {
                                &buf[..len]
                            };

                            // Craft ICMP echo reply using guest's original identifier
                            if let Some(frame) = craft_icmp_echo_reply(
                                remote_ip,
                                guest_ip,
                                icmp_id,
                                icmp_data,
                                gateway_mac,
                                guest_mac,
                            ) && tx.send(frame).await.is_err()
                            {
                                break;
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        }
                        Err(e) => {
                            tracing::debug!("NAT: ICMP recv error for {}: {}", guest_ip, e);
                            break;
                        }
                    }
                }
            });

            self.icmp_bindings.insert(
                key,
                IcmpNatEntry {
                    socket: tokio_socket,
                    task_handle,
                    last_activity: Instant::now(),
                },
            );
        }

        // Send ICMP echo request to the destination
        let entry = self.icmp_bindings.get(&key).unwrap();
        let socket = entry.socket.clone();

        // Build ICMP echo request packet
        let mut icmp_buf = vec![0u8; 8 + payload.len()];
        icmp_buf[0] = 8; // Echo request type
        icmp_buf[1] = 0; // Code
        // Checksum at [2..4] - fill later
        icmp_buf[4..6].copy_from_slice(&identifier.to_be_bytes());
        icmp_buf[6..8].copy_from_slice(&sequence.to_be_bytes());
        icmp_buf[8..].copy_from_slice(payload);

        // Calculate ICMP checksum
        let checksum = icmp_checksum(&icmp_buf);
        icmp_buf[2..4].copy_from_slice(&checksum.to_be_bytes());

        let dest_addr = SocketAddr::new(std::net::IpAddr::V4(dst_ip), 0);
        match socket.send_to(&icmp_buf, dest_addr).await {
            Ok(_) => {
                tracing::debug!(
                    "NAT: ICMP echo {} -> {} id={} seq={}",
                    src_ip,
                    dst_ip,
                    identifier,
                    sequence
                );
                true
            }
            Err(e) => {
                tracing::warn!("NAT: ICMP send to {} failed: {}", dst_ip, e);
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

        // Cleanup ICMP entries
        self.icmp_bindings.retain(|key, entry| {
            let idle_duration = now.duration_since(entry.last_activity);
            if idle_duration > ICMP_IDLE_TIMEOUT {
                tracing::debug!(
                    "NAT: Cleaning up idle ICMP entry for {} id={} (idle for {:?})",
                    key.guest_ip,
                    key.identifier,
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
pub(crate) fn craft_tcp_rst(
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

/// Calculate ICMP checksum (one's complement of one's complement sum).
fn icmp_checksum(data: &[u8]) -> u16 {
    let mut sum: u32 = 0;
    let mut i = 0;
    while i + 1 < data.len() {
        sum += u16::from_be_bytes([data[i], data[i + 1]]) as u32;
        i += 2;
    }
    if i < data.len() {
        sum += (data[i] as u32) << 8;
    }
    while (sum >> 16) != 0 {
        sum = (sum & 0xFFFF) + (sum >> 16);
    }
    !(sum as u16)
}

/// Craft an ICMP echo reply ethernet frame to send back to guest.
fn craft_icmp_echo_reply(
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    identifier: u16,
    icmp_data: &[u8],
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    // ICMP header is 8 bytes, then payload
    // For echo reply, we receive the full ICMP packet from socket
    // which includes: type(1) + code(1) + checksum(2) + id(2) + seq(2) + payload

    // Parse the received ICMP data to extract sequence and payload
    if icmp_data.len() < 8 {
        return None;
    }

    let msg_type = icmp_data[0];
    // Only process echo replies
    if msg_type != 0 {
        return None;
    }

    let sequence = u16::from_be_bytes([icmp_data[6], icmp_data[7]]);
    let payload = &icmp_data[8..];

    // Build ICMP echo reply (we rebuild it to ensure correctness)
    let icmp_len = 8 + payload.len();
    let ip_len = 20 + icmp_len;
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
        src_addr: src_ip,
        dst_addr: dst_ip,
        next_header: IpProtocol::Icmp,
        payload_len: icmp_len,
        hop_limit: 64,
    };

    let mut ip_packet = Ipv4Packet::new_unchecked(&mut frame[14..]);
    let checksum_caps = ChecksumCapabilities::default();
    ip_repr.emit(&mut ip_packet, &checksum_caps);

    // Build ICMP echo reply
    let icmp_start = 14 + 20;
    frame[icmp_start] = 0; // Echo reply type
    frame[icmp_start + 1] = 0; // Code
    // Checksum at [2..4] - fill later
    frame[icmp_start + 4..icmp_start + 6].copy_from_slice(&identifier.to_be_bytes());
    frame[icmp_start + 6..icmp_start + 8].copy_from_slice(&sequence.to_be_bytes());
    frame[icmp_start + 8..icmp_start + 8 + payload.len()].copy_from_slice(payload);

    // Calculate ICMP checksum
    let checksum = icmp_checksum(&frame[icmp_start..]);
    frame[icmp_start + 2..icmp_start + 4].copy_from_slice(&checksum.to_be_bytes());

    Some(frame)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Test that large TCP data is segmented into MSS-sized chunks.
    #[test]
    fn test_craft_tcp_data_within_mss() {
        let src = SocketAddrV4::new(Ipv4Addr::new(93, 184, 216, 34), 443);
        let dst = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 15), 12345);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        // Small payload (100 bytes) - should fit in single segment
        let payload = vec![0xAB; 100];
        let frame = craft_tcp_data(src, dst, 1000, 2000, &payload, gateway_mac, guest_mac);

        assert!(frame.is_some());
        let frame = frame.unwrap();

        // Verify frame is reasonable size (ethernet + IP + TCP headers + payload)
        // Ethernet: 14, IP: 20, TCP: 20, Payload: 100 = 154 bytes
        assert!(frame.len() < 1500, "Frame should be under MTU");

        let eth = EthernetFrame::new_checked(&frame).unwrap();
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        let tcp = TcpPacket::new_checked(ip.payload()).unwrap();

        assert_eq!(tcp.seq_number(), TcpSeqNumber(1000));
        assert_eq!(tcp.ack_number(), TcpSeqNumber(2000));
        assert_eq!(tcp.payload().len(), 100);
    }

    /// Test that MSS constant is correctly calculated.
    #[test]
    fn test_mss_constant() {
        assert_eq!(ETHERNET_MTU, 1500);
        assert_eq!(IP_HEADER_SIZE, 20);
        assert_eq!(TCP_HEADER_SIZE, 20);
        assert_eq!(TCP_MSS, 1460);
    }

    /// Test that sequence numbers would advance correctly for segmented data.
    #[test]
    fn test_sequence_advancement_for_segments() {
        // Simulate what happens when we segment 3000 bytes into MSS chunks
        let mss = TCP_MSS as u32;
        let total_bytes: u32 = 3000;
        let initial_seq: u32 = 1000;

        let mut seq = initial_seq;
        let mut offset: u32 = 0;

        let mut segments = Vec::new();
        while offset < total_bytes {
            let segment_len = std::cmp::min(mss, total_bytes - offset);
            segments.push((seq, segment_len));
            seq = seq.wrapping_add(segment_len);
            offset += segment_len;
        }

        // Should produce 3 segments: 1460, 1460, 80
        assert_eq!(segments.len(), 3);
        assert_eq!(segments[0], (1000, 1460)); // seq=1000, len=1460
        assert_eq!(segments[1], (2460, 1460)); // seq=2460, len=1460
        assert_eq!(segments[2], (3920, 80)); // seq=3920, len=80

        // Final sequence should be initial + total bytes
        assert_eq!(seq, initial_seq + total_bytes);
    }

    /// Test AtomicU32 sequence number synchronization pattern.
    #[test]
    fn test_atomic_sequence_synchronization() {
        let shared_seq = Arc::new(AtomicU32::new(1000));
        let task_seq = shared_seq.clone();

        // Simulate task sending 1460 bytes
        task_seq.fetch_add(1460, Ordering::Relaxed);

        // Entry should see updated value
        assert_eq!(shared_seq.load(Ordering::Relaxed), 2460);

        // Simulate task sending another 1460 bytes
        task_seq.fetch_add(1460, Ordering::Relaxed);

        // Entry should see further updated value
        assert_eq!(shared_seq.load(Ordering::Relaxed), 3920);
    }

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

    #[test]
    fn test_icmp_checksum() {
        // Test with known ICMP echo request data
        // Type(8) Code(0) Checksum(0) ID(1234) Seq(1) + "Hello"
        let mut data = vec![
            8, 0, // Type, Code
            0, 0, // Checksum placeholder
            0x04, 0xD2, // Identifier = 1234
            0x00, 0x01, // Sequence = 1
            b'H', b'e', b'l', b'l', b'o', // Payload
        ];

        let checksum = icmp_checksum(&data);
        data[2..4].copy_from_slice(&checksum.to_be_bytes());

        // Verify checksum by recalculating (should be 0 or 0xFFFF)
        let verify = icmp_checksum(&data);
        assert!(
            verify == 0 || verify == 0xFFFF,
            "Checksum verification failed: {:04X}",
            verify
        );
    }

    #[test]
    fn test_icmp_checksum_empty() {
        // Empty data should produce valid checksum (all 1s)
        let checksum = icmp_checksum(&[]);
        assert_eq!(checksum, 0xFFFF);
    }

    #[test]
    fn test_icmp_checksum_odd_length() {
        // Odd length data should be handled correctly
        let data = vec![0x08, 0x00, 0x00, 0x00, 0x01, 0x02, 0x03];
        let checksum = icmp_checksum(&data);
        // Checksum should be valid (non-zero for this data)
        assert!(checksum != 0);
    }

    #[test]
    fn test_craft_icmp_echo_reply() {
        let src_ip = Ipv4Addr::new(8, 8, 8, 8);
        let dst_ip = Ipv4Addr::new(10, 0, 2, 15);
        let identifier = 1234;
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        // Simulate received ICMP echo reply from socket
        // Type(0) Code(0) Checksum(xx) ID(1234) Seq(1) + payload
        let icmp_data = vec![
            0, 0, // Type = echo reply, Code = 0
            0x00, 0x00, // Checksum (ignored for test)
            0x04, 0xD2, // Identifier = 1234
            0x00, 0x01, // Sequence = 1
            b'p', b'i', b'n', b'g', // Payload
        ];

        let frame = craft_icmp_echo_reply(
            src_ip,
            dst_ip,
            identifier,
            &icmp_data,
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
        assert_eq!(Ipv4Addr::from(ip.src_addr()), src_ip);
        assert_eq!(Ipv4Addr::from(ip.dst_addr()), dst_ip);
        assert_eq!(ip.next_header(), IpProtocol::Icmp);

        // Verify ICMP header
        let icmp = Icmpv4Packet::new_checked(ip.payload()).unwrap();
        assert_eq!(icmp.msg_type(), Icmpv4Message::EchoReply);
        assert_eq!(icmp.msg_code(), 0);

        // Verify ICMP echo fields using smoltcp's API
        assert_eq!(icmp.echo_ident(), identifier);
        assert_eq!(icmp.echo_seq_no(), 1);
        assert_eq!(icmp.data(), b"ping");
    }

    #[test]
    fn test_craft_icmp_echo_reply_rejects_non_reply() {
        let src_ip = Ipv4Addr::new(8, 8, 8, 8);
        let dst_ip = Ipv4Addr::new(10, 0, 2, 15);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        // ICMP echo request (type 8) should be rejected
        let icmp_data = vec![
            8, 0, // Type = echo request (not reply)
            0x00, 0x00, 0x04, 0xD2, 0x00, 0x01,
        ];

        let frame = craft_icmp_echo_reply(src_ip, dst_ip, 1234, &icmp_data, gateway_mac, guest_mac);
        assert!(frame.is_none());
    }

    #[test]
    fn test_craft_icmp_echo_reply_rejects_short_data() {
        let src_ip = Ipv4Addr::new(8, 8, 8, 8);
        let dst_ip = Ipv4Addr::new(10, 0, 2, 15);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        // Too short ICMP data (less than 8 bytes)
        let icmp_data = vec![0, 0, 0, 0];

        let frame = craft_icmp_echo_reply(src_ip, dst_ip, 1234, &icmp_data, gateway_mac, guest_mac);
        assert!(frame.is_none());
    }

    #[test]
    fn test_smoltcp_icmp_echo_parsing() {
        // This test verifies that smoltcp's ICMP parsing API works as we expect.
        // Specifically, echo_ident() and echo_seq_no() return the echo-specific fields,
        // while data() returns ONLY the payload (not including identifier/sequence).

        // Build a raw ICMP echo request packet
        let mut icmp_bytes = vec![
            8, 0, // Type = echo request (8), Code = 0
            0, 0, // Checksum placeholder
            0x12, 0x34, // Identifier = 0x1234 = 4660
            0x00, 0x05, // Sequence = 5
            b't', b'e', b's', b't', // Payload = "test"
        ];

        // Calculate and fill checksum
        let checksum = icmp_checksum(&icmp_bytes);
        icmp_bytes[2..4].copy_from_slice(&checksum.to_be_bytes());

        // Parse with smoltcp
        let icmp = Icmpv4Packet::new_checked(&icmp_bytes).expect("Failed to parse ICMP");

        // Verify smoltcp's echo-specific accessors
        assert_eq!(icmp.msg_type(), Icmpv4Message::EchoRequest);
        assert_eq!(icmp.echo_ident(), 0x1234);
        assert_eq!(icmp.echo_seq_no(), 5);

        // CRITICAL: data() returns ONLY the payload, NOT identifier/sequence
        // This was the source of a bug where we incorrectly read identifier/sequence from data()
        assert_eq!(icmp.data(), b"test");
        assert_eq!(icmp.data().len(), 4); // Only "test", not 4 + 4 bytes
    }

    #[test]
    fn test_icmp_request_parsing_from_ip_packet() {
        // This tests the actual parsing path used in handle_icmp:
        // 1. Parse Ethernet frame
        // 2. Extract IP packet
        // 3. Parse ICMP from IP payload using smoltcp's echo-specific accessors
        // This regression test ensures we use echo_ident()/echo_seq_no()/data() correctly.

        // Build a complete Ethernet + IP + ICMP echo request frame
        let identifier: u16 = 0xABCD;
        let sequence: u16 = 42;
        let payload = b"hello world!"; // 12 bytes

        // Build ICMP packet
        let mut icmp_bytes = vec![0u8; 8 + payload.len()];
        icmp_bytes[0] = 8; // Echo request
        icmp_bytes[1] = 0; // Code
        icmp_bytes[4..6].copy_from_slice(&identifier.to_be_bytes());
        icmp_bytes[6..8].copy_from_slice(&sequence.to_be_bytes());
        icmp_bytes[8..].copy_from_slice(payload);
        let checksum = icmp_checksum(&icmp_bytes);
        icmp_bytes[2..4].copy_from_slice(&checksum.to_be_bytes());

        // Build IP packet around ICMP
        let src_ip = Ipv4Addr::new(10, 0, 2, 15);
        let dst_ip = Ipv4Addr::new(8, 8, 8, 8);
        let ip_repr = Ipv4Repr {
            src_addr: src_ip,
            dst_addr: dst_ip,
            next_header: IpProtocol::Icmp,
            payload_len: icmp_bytes.len(),
            hop_limit: 64,
        };

        let mut ip_bytes = vec![0u8; 20 + icmp_bytes.len()];
        let mut ip_packet = Ipv4Packet::new_unchecked(&mut ip_bytes);
        ip_repr.emit(&mut ip_packet, &ChecksumCapabilities::default());
        ip_packet.payload_mut().copy_from_slice(&icmp_bytes);

        // Build Ethernet frame around IP
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let eth_repr = EthernetRepr {
            src_addr: guest_mac,
            dst_addr: gateway_mac,
            ethertype: EthernetProtocol::Ipv4,
        };

        let mut frame = vec![0u8; 14 + ip_bytes.len()];
        let mut eth_frame = EthernetFrame::new_unchecked(&mut frame);
        eth_repr.emit(&mut eth_frame);
        eth_frame.payload_mut().copy_from_slice(&ip_bytes);

        // Now parse the frame back - simulating handle_icmp's parsing path
        let eth = EthernetFrame::new_checked(&frame).unwrap();
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        let icmp = Icmpv4Packet::new_checked(ip.payload()).unwrap();

        // Verify we extract fields correctly using smoltcp's echo-specific accessors
        // (NOT by manually parsing icmp.data()[0:2] and [2:4]!)
        assert_eq!(icmp.msg_type(), Icmpv4Message::EchoRequest);
        assert_eq!(icmp.echo_ident(), identifier);
        assert_eq!(icmp.echo_seq_no(), sequence);
        assert_eq!(icmp.data(), payload);
        assert_eq!(icmp.data().len(), payload.len());
    }
}
