//! Cluster network stack with DHCP and ARP for multi-VM networking.
//!
//! This provides the "gateway" functionality for a VirtualSwitch cluster:
//! - DHCP server for assigning IPs to VMs
//! - ARP responder for the gateway IP
//!
//! Unlike UserNatStack, this doesn't do NAT - VMs communicate directly
//! via L2 switching.

use crate::device::SmoltcpDevice;
use crate::dhcp::DhcpServer;
use crate::frame_io::FrameIO;

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::udp::{self, PacketBuffer, PacketMetadata};
use smoltcp::time::Instant;
use smoltcp::wire::{
    DhcpPacket, EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address,
};

use std::net::Ipv4Addr;
use std::time::Duration;

/// Configuration for the cluster network stack.
#[derive(Clone, Debug)]
pub struct ClusterStackConfig {
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

impl ClusterStackConfig {
    /// Create a config from subnet string (e.g., "10.0.3.0/24").
    pub fn from_subnet(subnet: &str, gateway: Option<Ipv4Addr>) -> Result<Self, String> {
        let (network, prefix) = subnet.split_once('/').ok_or("Invalid subnet format")?;
        let prefix: u8 = prefix.parse().map_err(|_| "Invalid prefix")?;
        let network_ip: Ipv4Addr = network.parse().map_err(|_| "Invalid network IP")?;

        let gateway_ip = gateway.unwrap_or_else(|| {
            let octets = network_ip.octets();
            Ipv4Addr::new(octets[0], octets[1], octets[2], 1)
        });

        let octets = network_ip.octets();
        let dhcp_start = Ipv4Addr::new(octets[0], octets[1], octets[2], 15);
        let dhcp_end = Ipv4Addr::new(octets[0], octets[1], octets[2], 254);

        Ok(Self {
            gateway_ip,
            subnet_prefix: prefix,
            dhcp_range_start: dhcp_start,
            dhcp_range_end: dhcp_end,
            gateway_mac: [0x52, 0x54, 0x00, 0xC0, 0x00, 0x01],
        })
    }
}

impl Default for ClusterStackConfig {
    fn default() -> Self {
        Self {
            gateway_ip: Ipv4Addr::new(10, 0, 3, 1),
            subnet_prefix: 24,
            dhcp_range_start: Ipv4Addr::new(10, 0, 3, 15),
            dhcp_range_end: Ipv4Addr::new(10, 0, 3, 254),
            gateway_mac: [0x52, 0x54, 0x00, 0xC0, 0x00, 0x01],
        }
    }
}

/// Cluster network stack providing DHCP and ARP for a VirtualSwitch.
///
/// This runs the "gateway" on the cluster network that:
/// - Responds to ARP requests for the gateway IP
/// - Runs a DHCP server to assign IPs to VMs
pub struct ClusterStack<F: FrameIO> {
    device: SmoltcpDevice<F>,
    iface: Interface,
    sockets: SocketSet<'static>,
    dhcp_handle: SocketHandle,
    dhcp_server: DhcpServer,
    start_time: std::time::Instant,
}

impl<F: FrameIO> ClusterStack<F> {
    /// Create a new cluster stack.
    pub fn new(frame_io: F, config: ClusterStackConfig) -> Self {
        let mut device = SmoltcpDevice::new(frame_io);
        let start_time = std::time::Instant::now();

        let hw_addr = HardwareAddress::Ethernet(EthernetAddress(config.gateway_mac));
        let iface_config = Config::new(hw_addr);
        let mut iface = Interface::new(iface_config, &mut device, smoltcp_now(start_time));

        iface.update_ip_addrs(|addrs| {
            addrs
                .push(IpCidr::new(
                    IpAddress::Ipv4(config.gateway_ip),
                    config.subnet_prefix,
                ))
                .ok();
        });

        let mut sockets = SocketSet::new(vec![]);

        let dhcp_rx_buffer = PacketBuffer::new(vec![PacketMetadata::EMPTY; 4], vec![0u8; 1500 * 4]);
        let dhcp_tx_buffer = PacketBuffer::new(vec![PacketMetadata::EMPTY; 4], vec![0u8; 1500 * 4]);
        let mut dhcp_socket = udp::Socket::new(dhcp_rx_buffer, dhcp_tx_buffer);
        dhcp_socket.bind(67).expect("Failed to bind DHCP socket");
        let dhcp_handle = sockets.add(dhcp_socket);

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
            start_time,
        }
    }

    /// Run the cluster stack.
    pub async fn run(mut self) {
        let mut interval = tokio::time::interval(Duration::from_millis(1));

        loop {
            interval.tick().await;

            {
                let waker = futures::task::noop_waker();
                let mut cx = std::task::Context::from_waker(&waker);
                self.device.poll_recv(&mut cx);
            }

            {
                let timestamp = smoltcp_now(self.start_time);
                self.iface
                    .poll(timestamp, &mut self.device, &mut self.sockets);
                self.process_dhcp();
            }
        }
    }

    fn process_dhcp(&mut self) {
        let socket = self.sockets.get_mut::<udp::Socket>(self.dhcp_handle);

        while let Ok((data, _endpoint)) = socket.recv() {
            if let Ok(dhcp_packet) = DhcpPacket::new_checked(data) {
                let client_mac = dhcp_packet.client_hardware_address();

                if let Some(response) = self.dhcp_server.handle_packet(client_mac, &dhcp_packet) {
                    let mut response_buf = vec![0u8; 576];
                    if let Ok(mut response_packet) = DhcpPacket::new_checked(&mut response_buf[..])
                        && response.emit(&mut response_packet).is_ok()
                    {
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

fn smoltcp_now(start: std::time::Instant) -> Instant {
    let elapsed = start.elapsed();
    Instant::from_millis(elapsed.as_millis() as i64)
}
