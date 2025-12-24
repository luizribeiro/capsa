use crate::device::SmoltcpDevice;
use crate::dhcp::DhcpServer;
use crate::error::NetError;
use crate::frame_io::FrameIO;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::udp::{self, PacketBuffer, PacketMetadata};
use smoltcp::time::Instant;
use smoltcp::wire::{
    DhcpPacket, EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address,
};

use std::collections::HashMap;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use tokio::net::{TcpStream, UdpSocket};

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
}

impl Default for StackConfig {
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

/// The main userspace NAT stack.
///
/// This runs the smoltcp interface and handles:
/// - ARP (automatic via smoltcp)
/// - ICMP echo (automatic via smoltcp)
/// - DHCP server
/// - TCP NAT (connection tracking + forwarding)
/// - UDP NAT
pub struct UserNatStack<F: FrameIO> {
    device: SmoltcpDevice<F>,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp_handle: SocketHandle,
    dhcp_server: DhcpServer,
    #[allow(dead_code)]
    config: StackConfig,
    #[allow(dead_code)]
    tcp_connections: HashMap<TcpConnectionKey, TcpConnection>,
    #[allow(dead_code)]
    udp_sockets: HashMap<UdpKey, UdpState>,
    start_time: std::time::Instant,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct TcpConnectionKey {
    guest_addr: SocketAddrV4,
    remote_addr: SocketAddrV4,
}

#[allow(dead_code)]
struct TcpConnection {
    socket_handle: SocketHandle,
    host_stream: Option<TcpStream>,
    state: TcpState,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[allow(dead_code)]
enum TcpState {
    Connecting,
    Connected,
    Closing,
}

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct UdpKey {
    guest_addr: SocketAddrV4,
}

#[allow(dead_code)]
struct UdpState {
    socket: UdpSocket,
    last_remote: SocketAddr,
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
                    IpAddress::Ipv4(config.gateway_ip.into()),
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

        Self {
            device,
            iface,
            sockets,
            dhcp_handle,
            dhcp_server,
            config,
            tcp_connections: HashMap::new(),
            udp_sockets: HashMap::new(),
            start_time,
        }
    }

    /// Run the network stack.
    ///
    /// This is an async function that should be spawned as a task.
    /// It runs until an error occurs or the frame I/O is closed.
    pub async fn run(mut self) -> Result<(), NetError> {
        let mut interval = tokio::time::interval(Duration::from_millis(1));
        // TODO: Optimize with proper async wakeups instead of fixed polling

        loop {
            interval.tick().await;

            // Synchronous processing block - no awaits allowed here
            {
                let waker = futures::task::noop_waker();
                let mut cx = std::task::Context::from_waker(&waker);

                // Poll for incoming frames
                self.device.poll_recv(&mut cx);

                // Process the smoltcp interface
                let timestamp = smoltcp_now(self.start_time);
                self.iface
                    .poll(timestamp, &mut self.device, &mut self.sockets);

                // Handle DHCP
                self.process_dhcp();
            }

            // Async processing - TCP and UDP NAT
            self.process_tcp().await;
            self.process_udp().await;
        }
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
                    {
                        if response.emit(&mut response_packet).is_ok() {
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
    }

    async fn process_tcp(&mut self) {
        // TODO: Implement TCP NAT
        // For each smoltcp TCP socket:
        // 1. If new connection (SYN received), open host TCP socket
        // 2. Forward data bidirectionally
        // 3. Handle connection close
    }

    async fn process_udp(&mut self) {
        // TODO: Implement UDP NAT
        // For each outbound UDP packet:
        // 1. Get or create host UDP socket
        // 2. Forward packet
        // 3. Forward any responses back
    }
}

/// Convert system time to smoltcp Instant
fn smoltcp_now(start: std::time::Instant) -> Instant {
    let elapsed = start.elapsed();
    Instant::from_millis(elapsed.as_millis() as i64)
}
