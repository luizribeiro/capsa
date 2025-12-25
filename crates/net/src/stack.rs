use crate::device::SmoltcpDevice;
use crate::dhcp::DhcpServer;
use crate::error::NetError;
use crate::frame_io::FrameIO;
use crate::nat::{FrameReceiver, NatTable, craft_tcp_rst, frame_channel};
use crate::policy::{PacketProtocol, PolicyChecker, PolicyResult};
use crate::port_forward::PortForwarder;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::udp::{self, PacketBuffer, PacketMetadata};
use smoltcp::time::Instant;
use smoltcp::wire::{
    DhcpPacket, EthernetAddress, EthernetFrame, EthernetProtocol, HardwareAddress, IpAddress,
    IpCidr, IpEndpoint, IpProtocol, Ipv4Address, Ipv4Packet, TcpPacket,
};

use std::net::SocketAddrV4;

use std::net::Ipv4Addr;
use std::time::Duration;

/// How often to run NAT cleanup (in milliseconds).
/// With 1ms polling intervals, 10000 means every 10 seconds.
const NAT_CLEANUP_INTERVAL_MS: u32 = 10_000;

/// Port forwarding rule.
#[derive(Clone, Debug)]
pub struct PortForwardRule {
    pub host_port: u16,
    pub guest_port: u16,
    pub is_tcp: bool,
}

/// Configuration for the userspace NAT stack.
#[derive(Clone, Debug)]
pub struct StackConfig {
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
    /// Port forwarding rules
    pub port_forwards: Vec<PortForwardRule>,
    /// Network filtering policy
    pub policy: Option<capsa_core::NetworkPolicy>,
}

impl Default for StackConfig {
    fn default() -> Self {
        Self {
            gateway_ip: Ipv4Addr::new(10, 0, 2, 2),
            subnet_prefix: 24,
            dhcp_range_start: Ipv4Addr::new(10, 0, 2, 15),
            dhcp_range_end: Ipv4Addr::new(10, 0, 2, 254),
            gateway_mac: [0x52, 0x54, 0x00, 0x00, 0x00, 0x01],
            port_forwards: Vec::new(),
            policy: None,
        }
    }
}

/// The main userspace NAT stack.
///
/// This runs the smoltcp interface and handles:
/// - ARP (automatic via smoltcp)
/// - ICMP echo (automatic via smoltcp)
/// - DHCP server
/// - TCP NAT (connection tracking + forwarding)
/// - UDP NAT (connection tracking + forwarding)
/// - Port forwarding (host → guest)
/// - Network policy enforcement
pub struct UserNatStack<F: FrameIO> {
    device: SmoltcpDevice<F>,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp_handle: SocketHandle,
    dhcp_server: DhcpServer,
    config: StackConfig,
    nat: NatTable,
    nat_rx: FrameReceiver,
    port_forwarder: Option<PortForwarder>,
    policy_checker: Option<PolicyChecker>,
    start_time: std::time::Instant,
}

impl<F: FrameIO> UserNatStack<F> {
    /// Create a new userspace NAT stack.
    pub fn new(frame_io: F, config: StackConfig) -> Self {
        let mut device = SmoltcpDevice::new(frame_io);
        let start_time = std::time::Instant::now();

        // Create the smoltcp interface
        let hw_addr = HardwareAddress::Ethernet(EthernetAddress(config.gateway_mac));
        let iface_config = Config::new(hw_addr);
        let mut iface = Interface::new(iface_config, &mut device, smoltcp_now(start_time));

        // Configure interface IP
        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(
                    IpAddress::Ipv4(config.gateway_ip),
                    config.subnet_prefix,
                ))
                .ok();
        });

        // Create socket set
        let mut sockets = SocketSet::new(vec![]);

        // Create UDP socket for DHCP (port 67)
        let dhcp_rx_buffer = PacketBuffer::new(vec![PacketMetadata::EMPTY; 4], vec![0u8; 1500 * 4]);
        let dhcp_tx_buffer = PacketBuffer::new(vec![PacketMetadata::EMPTY; 4], vec![0u8; 1500 * 4]);
        let mut dhcp_socket = udp::Socket::new(dhcp_rx_buffer, dhcp_tx_buffer);
        dhcp_socket.bind(67).expect("Failed to bind DHCP socket");
        let dhcp_handle = sockets.add(dhcp_socket);

        // Create DHCP server
        let dhcp_server = DhcpServer::new(
            config.gateway_ip,
            config.subnet_prefix,
            config.dhcp_range_start,
            config.dhcp_range_end,
        );

        // Create NAT table with response channel
        let (nat_tx, nat_rx) = frame_channel(256);
        let nat = NatTable::new(config.gateway_ip, config.gateway_mac, nat_tx.clone());

        // Create port forwarder if there are any port forward rules
        let port_forwarder = if config.port_forwards.is_empty() {
            None
        } else {
            Some(PortForwarder::new(
                nat_tx,
                config.gateway_ip,
                config.gateway_mac,
                config.dhcp_range_start,
            ))
        };

        // Create policy checker if policy is configured
        let policy_checker = config
            .policy
            .as_ref()
            .map(|p| PolicyChecker::new(p.default_action, &p.rules));

        Self {
            device,
            iface,
            sockets,
            dhcp_handle,
            dhcp_server,
            config,
            nat,
            nat_rx,
            port_forwarder,
            policy_checker,
            start_time,
        }
    }

    /// Run the network stack.
    ///
    /// This is an async function that should be spawned as a task.
    /// It runs until an error occurs or the frame I/O is closed.
    pub async fn run(mut self) -> Result<(), NetError> {
        // Start port forward listeners
        if let Some(ref mut pf) = self.port_forwarder {
            for rule in &self.config.port_forwards {
                if rule.is_tcp {
                    if let Err(e) = pf.start_tcp_forward(rule.host_port, rule.guest_port).await {
                        tracing::warn!(
                            "Failed to start TCP port forward {}:{}: {}",
                            rule.host_port,
                            rule.guest_port,
                            e
                        );
                    }
                } else if let Err(e) = pf.start_udp_forward(rule.host_port, rule.guest_port).await {
                    tracing::warn!(
                        "Failed to start UDP port forward {}:{}: {}",
                        rule.host_port,
                        rule.guest_port,
                        e
                    );
                }
            }
        }

        let mut interval = tokio::time::interval(Duration::from_millis(1));
        let mut cleanup_counter = 0u32;

        loop {
            interval.tick().await;

            // Receive frames from guest
            {
                let waker = futures::task::noop_waker();
                let mut cx = std::task::Context::from_waker(&waker);
                self.device.poll_recv(&mut cx);
            }

            // Check for NAT response frames to send back to guest
            while let Ok(frame) = self.nat_rx.try_recv() {
                if let Err(e) = self.device.send_frame(&frame) {
                    tracing::warn!("Failed to send NAT response frame: {}", e);
                }
            }

            // Check if we have a frame destined to gateway (potential port forward response)
            if let Some(frame) = self.device.peek_rx() {
                // Check if this is a port forward response
                if let Some(ref pf) = self.port_forwarder
                    && self.is_port_forward_response(frame)
                {
                    let frame_copy = frame.to_vec();
                    self.device.discard_rx();
                    pf.handle_guest_response(&frame_copy).await;
                    continue;
                }

                // Check if destined for external IP
                if self.is_external_destination(frame) {
                    // Apply network policy if configured
                    if let Some(ref checker) = self.policy_checker
                        && let Some(info) = PolicyChecker::extract_packet_info(frame)
                    {
                        match checker.check(&info) {
                            PolicyResult::Deny => {
                                tracing::debug!(
                                    "Policy denied: {:?} {} -> {}:{}",
                                    info.protocol,
                                    info.src_ip,
                                    info.dst_ip,
                                    info.dst_port.unwrap_or(0)
                                );
                                // Send TCP RST for denied TCP SYN to avoid guest waiting for timeout
                                if info.protocol == PacketProtocol::Tcp {
                                    let frame_copy = frame.to_vec();
                                    self.device.discard_rx();
                                    self.send_rst_for_denied_packet(&frame_copy);
                                    continue;
                                }
                                self.device.discard_rx();
                                continue;
                            }
                            PolicyResult::Log => {
                                tracing::info!(
                                    "Policy logged: {} -> {}:{}",
                                    info.src_ip,
                                    info.dst_ip,
                                    info.dst_port.unwrap_or(0)
                                );
                            }
                            PolicyResult::Allow => {}
                        }
                    }

                    let frame_copy = frame.to_vec();
                    self.device.discard_rx();
                    self.nat.process_frame(&frame_copy).await;
                    continue;
                }
            }

            // Process with smoltcp (ARP, ICMP, DHCP)
            {
                let timestamp = smoltcp_now(self.start_time);
                self.iface
                    .poll(timestamp, &mut self.device, &mut self.sockets);
                self.process_dhcp();
            }

            // Periodic cleanup of idle NAT entries
            cleanup_counter = cleanup_counter.wrapping_add(1);
            if cleanup_counter.is_multiple_of(NAT_CLEANUP_INTERVAL_MS) {
                self.nat.cleanup();
            }
        }
    }

    /// Check if a frame is a potential port forward response (from guest to gateway).
    fn is_port_forward_response(&self, frame: &[u8]) -> bool {
        let Ok(eth_frame) = EthernetFrame::new_checked(frame) else {
            return false;
        };

        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return false;
        }

        let Ok(ip_packet) = Ipv4Packet::new_checked(eth_frame.payload()) else {
            return false;
        };

        let src_ip: Ipv4Addr = ip_packet.src_addr();
        let dst_ip: Ipv4Addr = ip_packet.dst_addr();

        // Frame from guest to gateway
        src_ip == self.config.dhcp_range_start && dst_ip == self.config.gateway_ip
    }

    /// Check if a frame is destined for an external IP (not our gateway or broadcast).
    fn is_external_destination(&self, frame: &[u8]) -> bool {
        let Ok(eth_frame) = EthernetFrame::new_checked(frame) else {
            return false;
        };

        // Non-IPv4 (ARP, etc.) should go to smoltcp
        if eth_frame.ethertype() != EthernetProtocol::Ipv4 {
            return false;
        }

        let Ok(ip_packet) = Ipv4Packet::new_checked(eth_frame.payload()) else {
            return false;
        };

        let dst_ip: Ipv4Addr = ip_packet.dst_addr();

        // Gateway IP - let smoltcp handle it (ICMP, DHCP requests to gateway)
        if dst_ip == self.config.gateway_ip {
            return false;
        }

        // Broadcast addresses should go to smoltcp
        if dst_ip.is_broadcast() || dst_ip.octets()[3] == 255 {
            return false;
        }

        // Multicast should go to smoltcp (or be dropped)
        if dst_ip.is_multicast() {
            return false;
        }

        // Local subnet broadcast (e.g., 10.0.2.255 for 10.0.2.0/24)
        // Check if it's the subnet broadcast address
        let subnet_mask = !((1u32 << (32 - self.config.subnet_prefix)) - 1);
        let subnet = u32::from_be_bytes(self.config.gateway_ip.octets()) & subnet_mask;
        let broadcast = subnet | !subnet_mask;
        if u32::from_be_bytes(dst_ip.octets()) == broadcast {
            return false;
        }

        // Everything else is external and should be NAT'd
        true
    }

    fn process_dhcp(&mut self) {
        let socket = self.sockets.get_mut::<udp::Socket>(self.dhcp_handle);

        while let Ok((data, _endpoint)) = socket.recv() {
            // Parse DHCP packet
            if let Ok(dhcp_packet) = DhcpPacket::new_checked(data) {
                // Extract client MAC from the DHCP packet's chaddr field
                let client_mac = dhcp_packet.client_hardware_address();

                if let Some(response) = self.dhcp_server.handle_packet(client_mac, &dhcp_packet) {
                    // Serialize and send response
                    // DHCP packets are typically around 300-400 bytes, 576 is safe minimum
                    let mut response_buf = vec![0u8; 576];
                    if let Ok(mut response_packet) = DhcpPacket::new_checked(&mut response_buf[..])
                        && response.emit(&mut response_packet).is_ok()
                    {
                        // The DHCP packet header is 240 bytes minimum, plus options
                        // We'll send the whole buffer since UDP doesn't care about trailing zeros
                        // and the client will parse based on the options
                        let dest = IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), 68);
                        if let Err(e) = socket.send_slice(&response_buf, dest) {
                            tracing::warn!("Failed to send DHCP response: {:?}", e);
                        }
                    }
                }
            }
        }
    }

    /// Send a TCP RST packet to the guest for a denied connection.
    /// This allows the guest to fail fast instead of waiting for TCP retransmit timeouts.
    fn send_rst_for_denied_packet(&mut self, frame: &[u8]) {
        let Ok(eth_frame) = EthernetFrame::new_checked(frame) else {
            return;
        };

        let Ok(ip_packet) = Ipv4Packet::new_checked(eth_frame.payload()) else {
            return;
        };

        if ip_packet.next_header() != IpProtocol::Tcp {
            return;
        }

        let Ok(tcp_packet) = TcpPacket::new_checked(ip_packet.payload()) else {
            return;
        };

        // Only send RST for SYN packets (new connection attempts)
        if !tcp_packet.syn() || tcp_packet.ack() {
            return;
        }

        let guest_mac = eth_frame.src_addr();
        let src_ip: Ipv4Addr = ip_packet.src_addr();
        let dst_ip: Ipv4Addr = ip_packet.dst_addr();
        let guest_addr = SocketAddrV4::new(src_ip, tcp_packet.src_port());
        let remote_addr = SocketAddrV4::new(dst_ip, tcp_packet.dst_port());
        let guest_seq = tcp_packet.seq_number().0 as u32;

        // Craft and send RST packet directly through device for immediate delivery
        if let Some(rst_frame) = craft_tcp_rst(
            remote_addr,
            guest_addr,
            0,
            guest_seq.wrapping_add(1),
            EthernetAddress(self.config.gateway_mac),
            guest_mac,
        ) {
            match self.device.send_frame(&rst_frame) {
                Ok(()) => {
                    tracing::debug!(
                        "Sent RST to guest {} for denied connection to {}",
                        guest_addr,
                        remote_addr
                    );
                }
                Err(e) => {
                    tracing::warn!("Failed to send RST for denied packet: {}", e);
                }
            }
        } else {
            tracing::warn!(
                "Failed to craft RST packet for {} -> {}",
                guest_addr,
                remote_addr
            );
        }
    }
}

/// Convert system time to smoltcp Instant
fn smoltcp_now(start: std::time::Instant) -> Instant {
    let elapsed = start.elapsed();
    Instant::from_millis(elapsed.as_millis() as i64)
}
