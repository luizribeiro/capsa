//! Port forwarding: host → guest connections.
//!
//! This module allows host applications to connect to services running inside
//! the guest VM. It listens on host ports and forwards connections to the guest.

use crate::nat::FrameSender;
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{
    EthernetAddress, EthernetFrame, EthernetProtocol, EthernetRepr, IpProtocol, Ipv4Address,
    Ipv4Repr, TcpPacket, TcpRepr, TcpSeqNumber, UdpPacket, UdpRepr,
};
use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::Mutex;
use tokio::task::JoinHandle;

/// Configuration for a single port forward.
#[derive(Clone, Debug)]
pub struct ForwardConfig {
    pub host_port: u16,
    pub guest_port: u16,
    pub is_tcp: bool,
}

/// Port forwarder managing host listeners and inbound connections.
pub struct PortForwarder {
    /// Channel to send frames to guest
    tx_to_guest: FrameSender,
    /// Gateway IP (source for crafted packets)
    gateway_ip: Ipv4Addr,
    /// Gateway MAC
    gateway_mac: EthernetAddress,
    /// Guest IP (destination for forwarded connections)
    guest_ip: Ipv4Addr,
    /// Guest MAC (learned from first inbound traffic)
    guest_mac: Arc<Mutex<Option<EthernetAddress>>>,
    /// Active inbound TCP connections: (virtual_port, host_client_addr) → connection state
    tcp_inbound: Arc<Mutex<HashMap<InboundKey, InboundTcpState>>>,
    /// Active inbound UDP bindings
    udp_inbound: Arc<Mutex<HashMap<u16, UdpInboundState>>>,
    /// Listener tasks
    listener_handles: Vec<JoinHandle<()>>,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct InboundKey {
    host_port: u16,
    virtual_port: u16,
}

struct InboundTcpState {
    #[allow(dead_code)]
    host_stream: TcpStream,
    our_seq: u32,
    guest_next_seq: u32,
    established: bool,
}

struct UdpInboundState {
    #[allow(dead_code)]
    socket: Arc<UdpSocket>,
    #[allow(dead_code)]
    task_handle: JoinHandle<()>,
}

impl PortForwarder {
    pub fn new(
        tx_to_guest: FrameSender,
        gateway_ip: Ipv4Addr,
        gateway_mac: [u8; 6],
        guest_ip: Ipv4Addr,
    ) -> Self {
        Self {
            tx_to_guest,
            gateway_ip,
            gateway_mac: EthernetAddress(gateway_mac),
            guest_ip,
            guest_mac: Arc::new(Mutex::new(None)),
            tcp_inbound: Arc::new(Mutex::new(HashMap::new())),
            udp_inbound: Arc::new(Mutex::new(HashMap::new())),
            listener_handles: Vec::new(),
        }
    }

    /// Set the guest MAC address (learned from ARP or DHCP).
    pub async fn set_guest_mac(&self, mac: [u8; 6]) {
        let mut guest_mac = self.guest_mac.lock().await;
        *guest_mac = Some(EthernetAddress(mac));
    }

    /// Start listening for a TCP port forward.
    pub async fn start_tcp_forward(
        &mut self,
        host_port: u16,
        guest_port: u16,
    ) -> std::io::Result<()> {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], host_port))).await?;

        let tx = self.tx_to_guest.clone();
        let gateway_ip = self.gateway_ip;
        let gateway_mac = self.gateway_mac;
        let guest_ip = self.guest_ip;
        let guest_mac = self.guest_mac.clone();
        let tcp_inbound = self.tcp_inbound.clone();

        let handle = tokio::spawn(async move {
            loop {
                match listener.accept().await {
                    Ok((stream, client_addr)) => {
                        tracing::debug!(
                            "Port forward: host client {} → guest:{}",
                            client_addr,
                            guest_port
                        );

                        // Assign a virtual source port for this connection
                        let virtual_port = (client_addr.port() % 16384) + 49152;

                        let key = InboundKey {
                            host_port,
                            virtual_port,
                        };

                        // Generate initial sequence number
                        let our_seq = rand::random::<u32>();

                        // Store connection state
                        {
                            let mut inbound = tcp_inbound.lock().await;
                            inbound.insert(
                                key,
                                InboundTcpState {
                                    host_stream: stream,
                                    our_seq,
                                    guest_next_seq: 0,
                                    established: false,
                                },
                            );
                        }

                        // Get guest MAC (or use broadcast if not learned yet)
                        let dst_mac = {
                            let mac = guest_mac.lock().await;
                            mac.unwrap_or(EthernetAddress([0xff, 0xff, 0xff, 0xff, 0xff, 0xff]))
                        };

                        // Craft SYN packet to guest
                        let src_addr = SocketAddrV4::new(gateway_ip, virtual_port);
                        let dst_addr = SocketAddrV4::new(guest_ip, guest_port);

                        if let Some(syn_frame) =
                            craft_tcp_syn(src_addr, dst_addr, our_seq, gateway_mac, dst_mac)
                        {
                            if let Err(e) = tx.send(syn_frame).await {
                                tracing::warn!("Failed to send SYN to guest: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("Port forward accept error: {}", e);
                    }
                }
            }
        });

        self.listener_handles.push(handle);
        Ok(())
    }

    /// Start listening for a UDP port forward.
    pub async fn start_udp_forward(
        &mut self,
        host_port: u16,
        guest_port: u16,
    ) -> std::io::Result<()> {
        let socket =
            Arc::new(UdpSocket::bind(SocketAddr::from(([127, 0, 0, 1], host_port))).await?);

        let tx = self.tx_to_guest.clone();
        let gateway_ip = self.gateway_ip;
        let gateway_mac = self.gateway_mac;
        let guest_ip = self.guest_ip;
        let guest_mac = self.guest_mac.clone();
        let socket_clone = socket.clone();
        let _udp_inbound = self.udp_inbound.clone();

        let handle = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            loop {
                match socket_clone.recv_from(&mut buf).await {
                    Ok((len, client_addr)) => {
                        // Virtual source port based on client
                        let virtual_port = (client_addr.port() % 16384) + 49152;

                        // Get guest MAC
                        let dst_mac = {
                            let mac = guest_mac.lock().await;
                            mac.unwrap_or(EthernetAddress([0xff, 0xff, 0xff, 0xff, 0xff, 0xff]))
                        };

                        // Craft UDP frame to guest
                        let src_addr = SocketAddrV4::new(gateway_ip, virtual_port);
                        let dst_addr = SocketAddrV4::new(guest_ip, guest_port);

                        if let Some(frame) =
                            craft_udp_frame(src_addr, dst_addr, &buf[..len], gateway_mac, dst_mac)
                        {
                            if let Err(e) = tx.send(frame).await {
                                tracing::warn!("Failed to send UDP to guest: {}", e);
                            }
                        }
                    }
                    Err(e) => {
                        tracing::warn!("UDP port forward recv error: {}", e);
                    }
                }
            }
        });

        // Store UDP state
        {
            let mut inbound = self.udp_inbound.lock().await;
            inbound.insert(
                host_port,
                UdpInboundState {
                    socket,
                    task_handle: handle,
                },
            );
        }

        Ok(())
    }

    /// Handle a response frame from guest (destined to gateway).
    /// Returns true if the frame was handled as a port forward response.
    pub async fn handle_guest_response(&self, frame: &[u8]) -> bool {
        let Ok(eth_frame) = EthernetFrame::new_checked(frame) else {
            return false;
        };

        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return false;
        }

        let Ok(ip_packet) = smoltcp::wire::Ipv4Packet::new_checked(eth_frame.payload()) else {
            return false;
        };

        let src_ip: Ipv4Addr = ip_packet.src_addr().into();
        let dst_ip: Ipv4Addr = ip_packet.dst_addr().into();

        // Only handle responses from our guest to gateway
        if src_ip != self.guest_ip || dst_ip != self.gateway_ip {
            return false;
        }

        // Learn guest MAC from this response
        {
            let mut guest_mac = self.guest_mac.lock().await;
            if guest_mac.is_none() {
                *guest_mac = Some(eth_frame.src_addr());
                tracing::debug!("Learned guest MAC: {:?}", eth_frame.src_addr());
            }
        }

        match ip_packet.next_header() {
            IpProtocol::Tcp => self.handle_tcp_response(&ip_packet).await,
            IpProtocol::Udp => self.handle_udp_response(&ip_packet).await,
            _ => false,
        }
    }

    async fn handle_tcp_response(&self, ip_packet: &smoltcp::wire::Ipv4Packet<&[u8]>) -> bool {
        let Ok(tcp_packet) = TcpPacket::new_checked(ip_packet.payload()) else {
            return false;
        };

        let dst_port = tcp_packet.dst_port();

        // Find matching inbound connection by virtual port
        let mut inbound = self.tcp_inbound.lock().await;
        let key = inbound.keys().find(|k| k.virtual_port == dst_port).copied();

        let Some(key) = key else {
            return false;
        };

        let state = inbound.get_mut(&key).unwrap();

        // Handle SYN-ACK (connection established)
        if tcp_packet.syn() && tcp_packet.ack() {
            state.guest_next_seq = tcp_packet.seq_number().0 as u32 + 1;
            state.established = true;

            // Send ACK to complete handshake
            let guest_mac = {
                let mac = self.guest_mac.lock().await;
                mac.unwrap_or(EthernetAddress([0xff, 0xff, 0xff, 0xff, 0xff, 0xff]))
            };

            let src_addr = SocketAddrV4::new(self.gateway_ip, key.virtual_port);
            let guest_port = tcp_packet.src_port();
            let dst_addr = SocketAddrV4::new(self.guest_ip, guest_port);

            if let Some(ack_frame) = craft_tcp_ack(
                src_addr,
                dst_addr,
                state.our_seq + 1,
                state.guest_next_seq,
                self.gateway_mac,
                guest_mac,
            ) {
                let _ = self.tx_to_guest.send(ack_frame).await;
            }

            tracing::debug!(
                "Port forward connection established for port {}",
                key.host_port
            );

            // Start bidirectional forwarding
            // TODO: Spawn forwarding task

            return true;
        }

        // Handle data
        if state.established && !tcp_packet.payload().is_empty() {
            // Forward data to host client
            // TODO: Write to host_stream
            state.guest_next_seq = state
                .guest_next_seq
                .wrapping_add(tcp_packet.payload().len() as u32);
        }

        // Handle FIN
        if tcp_packet.fin() {
            // TODO: Close connection
        }

        true
    }

    async fn handle_udp_response(&self, ip_packet: &smoltcp::wire::Ipv4Packet<&[u8]>) -> bool {
        let Ok(udp_packet) = UdpPacket::new_checked(ip_packet.payload()) else {
            return false;
        };

        let dst_port = udp_packet.dst_port();

        // Find matching UDP forwarder by mapping virtual port back to host port
        let _inbound = self.udp_inbound.lock().await;

        // For now, just log - full implementation would track client addresses
        tracing::debug!(
            "UDP response from guest port {} to virtual port {}",
            udp_packet.src_port(),
            dst_port
        );

        false
    }

    /// Stop all port forward listeners.
    pub fn stop(&mut self) {
        for handle in self.listener_handles.drain(..) {
            handle.abort();
        }
    }
}

impl Drop for PortForwarder {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Craft a TCP SYN frame to initiate connection to guest.
fn craft_tcp_syn(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    seq_num: u32,
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    // SYN with MSS option: 20 bytes header + 4 bytes MSS option = 24 bytes
    let tcp_len = 24;
    let ip_len = 20 + tcp_len;
    let total_len = 14 + ip_len;

    let mut frame = vec![0u8; total_len];

    let eth_repr = EthernetRepr {
        src_addr: gateway_mac,
        dst_addr: guest_mac,
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
    eth_repr.emit(&mut eth_frame);

    let ip_repr = Ipv4Repr {
        src_addr: Ipv4Address::from(*src_addr.ip()),
        dst_addr: Ipv4Address::from(*dst_addr.ip()),
        next_header: IpProtocol::Tcp,
        payload_len: tcp_len,
        hop_limit: 64,
    };

    let mut ip_packet = smoltcp::wire::Ipv4Packet::new_unchecked(&mut frame[14..]);
    let checksum_caps = ChecksumCapabilities::default();
    ip_repr.emit(&mut ip_packet, &checksum_caps);

    let tcp_repr = TcpRepr {
        src_port: src_addr.port(),
        dst_port: dst_addr.port(),
        seq_number: TcpSeqNumber(seq_num as i32),
        ack_number: None,
        window_len: 65535,
        window_scale: None,
        control: smoltcp::wire::TcpControl::Syn,
        max_seg_size: Some(1460),
        sack_permitted: false,
        sack_ranges: [None, None, None],
        timestamp: None,
        payload: &[],
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

/// Craft a TCP ACK frame.
fn craft_tcp_ack(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    seq_num: u32,
    ack_num: u32,
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    let tcp_len = 20;
    let ip_len = 20 + tcp_len;
    let total_len = 14 + ip_len;

    let mut frame = vec![0u8; total_len];

    let eth_repr = EthernetRepr {
        src_addr: gateway_mac,
        dst_addr: guest_mac,
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
    eth_repr.emit(&mut eth_frame);

    let ip_repr = Ipv4Repr {
        src_addr: Ipv4Address::from(*src_addr.ip()),
        dst_addr: Ipv4Address::from(*dst_addr.ip()),
        next_header: IpProtocol::Tcp,
        payload_len: tcp_len,
        hop_limit: 64,
    };

    let mut ip_packet = smoltcp::wire::Ipv4Packet::new_unchecked(&mut frame[14..]);
    let checksum_caps = ChecksumCapabilities::default();
    ip_repr.emit(&mut ip_packet, &checksum_caps);

    let tcp_repr = TcpRepr {
        src_port: src_addr.port(),
        dst_port: dst_addr.port(),
        seq_number: TcpSeqNumber(seq_num as i32),
        ack_number: Some(TcpSeqNumber(ack_num as i32)),
        window_len: 65535,
        window_scale: None,
        control: smoltcp::wire::TcpControl::None,
        max_seg_size: None,
        sack_permitted: false,
        sack_ranges: [None, None, None],
        timestamp: None,
        payload: &[],
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

/// Craft a UDP frame to send to guest.
fn craft_udp_frame(
    src_addr: SocketAddrV4,
    dst_addr: SocketAddrV4,
    payload: &[u8],
    gateway_mac: EthernetAddress,
    guest_mac: EthernetAddress,
) -> Option<Vec<u8>> {
    let udp_len = 8 + payload.len();
    let ip_len = 20 + udp_len;
    let total_len = 14 + ip_len;

    let mut frame = vec![0u8; total_len];

    let eth_repr = EthernetRepr {
        src_addr: gateway_mac,
        dst_addr: guest_mac,
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut eth_frame = EthernetFrame::new_unchecked(&mut frame[..]);
    eth_repr.emit(&mut eth_frame);

    let ip_repr = Ipv4Repr {
        src_addr: Ipv4Address::from(*src_addr.ip()),
        dst_addr: Ipv4Address::from(*dst_addr.ip()),
        next_header: IpProtocol::Udp,
        payload_len: udp_len,
        hop_limit: 64,
    };

    let mut ip_packet = smoltcp::wire::Ipv4Packet::new_unchecked(&mut frame[14..]);
    let checksum_caps = ChecksumCapabilities::default();
    ip_repr.emit(&mut ip_packet, &checksum_caps);

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
    fn craft_tcp_syn_valid() {
        let src = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 2), 50000);
        let dst = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 15), 80);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        let frame = craft_tcp_syn(src, dst, 12345, gateway_mac, guest_mac);
        assert!(frame.is_some());

        let frame = frame.unwrap();
        let eth = EthernetFrame::new_checked(&frame).unwrap();
        assert_eq!(eth.ethertype(), EthernetProtocol::Ipv4);

        let ip = smoltcp::wire::Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert_eq!(ip.next_header(), IpProtocol::Tcp);

        let tcp = TcpPacket::new_checked(ip.payload()).unwrap();
        assert!(tcp.syn());
        assert!(!tcp.ack());
        assert_eq!(tcp.src_port(), 50000);
        assert_eq!(tcp.dst_port(), 80);
    }

    #[test]
    fn craft_udp_frame_valid() {
        let src = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 2), 50000);
        let dst = SocketAddrV4::new(Ipv4Addr::new(10, 0, 2, 15), 53);
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);
        let payload = b"test data";

        let frame = craft_udp_frame(src, dst, payload, gateway_mac, guest_mac);
        assert!(frame.is_some());

        let frame = frame.unwrap();
        let eth = EthernetFrame::new_checked(&frame).unwrap();
        assert_eq!(eth.ethertype(), EthernetProtocol::Ipv4);

        let ip = smoltcp::wire::Ipv4Packet::new_checked(eth.payload()).unwrap();
        assert_eq!(ip.next_header(), IpProtocol::Udp);

        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        assert_eq!(udp.src_port(), 50000);
        assert_eq!(udp.dst_port(), 53);
        assert_eq!(udp.payload(), payload);
    }
}
