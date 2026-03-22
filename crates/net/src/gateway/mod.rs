mod bounded_queue;
mod config;
mod device;
mod dhcp;
pub(super) mod dns;
mod ingress;
pub(super) mod outbound_buffer;
mod run;
pub(super) mod tcp;

use crate::dns::DnsCache;
use crate::dns::DnsProxy;
use crate::frame::{
    frame_channel, smoltcp_now, spawn_frame_io_tasks, EthernetFrameIO, FrameReceiver,
};
use crate::nat::NatTable;

pub use self::config::GatewayStackConfig;
use self::device::SmoltcpDevice;
use self::dhcp::{DhcpEvent, DhcpServer};
use self::dns::{DnsDispatcher, DnsResponse, MAX_DNS_QUERIES};
use self::tcp::{TcpHostEvent, TcpManager};

use std::sync::{Arc, Mutex, RwLock};

use smoltcp::iface::{Config, Interface, SocketHandle, SocketSet};
use smoltcp::socket::udp::{self, PacketBuffer, PacketMetadata};
use smoltcp::wire::{
    DhcpPacket, EthernetAddress, HardwareAddress, IpAddress, IpCidr, IpEndpoint, Ipv4Address,
};

use tokio::sync::Semaphore;
use tokio::task::JoinHandle;

/// The main userspace NAT stack.
///
/// This runs the smoltcp interface and handles:
/// - ARP (automatic via smoltcp)
/// - ICMP echo (automatic via smoltcp)
/// - DHCP server
/// - DNS proxy
/// - TCP NAT (connection tracking + forwarding)
/// - UDP NAT (connection tracking + forwarding)
pub struct GatewayStack {
    pub(super) device: SmoltcpDevice,
    pub(super) iface: Interface,
    pub(super) sockets: SocketSet<'static>,
    dhcp_handle: SocketHandle,
    pub(super) dhcp_server: DhcpServer,
    pub(super) config: GatewayStackConfig,
    pub(super) dns: DnsDispatcher,
    pub(super) dns_response_rx: tokio::sync::mpsc::Receiver<DnsResponse>,
    pub(super) nat: NatTable,
    pub(super) nat_rx: FrameReceiver,
    pub(super) tcp_manager: TcpManager,
    pub(super) tcp_host_rx: tokio::sync::mpsc::Receiver<TcpHostEvent>,
    pub(super) tcp_host_tx: tokio::sync::mpsc::Sender<TcpHostEvent>,
    pub(super) start_time: std::time::Instant,
    /// Channel for receiving frames from the I/O task (bounded for backpressure).
    pub(super) rx_from_guest: FrameReceiver,
    /// Handles to the background I/O tasks (aborted on drop).
    /// Split into RX and TX tasks to avoid deadlocks from cross-direction blocking.
    io_tasks: Mutex<Option<(JoinHandle<()>, JoinHandle<()>)>>,
}

impl Drop for GatewayStack {
    fn drop(&mut self) {
        // Abort both I/O tasks - channels will be dropped, causing tasks to exit.
        // Use get_mut() to bypass the lock synchronously (safe because we have &mut self).
        if let Some((rx_handle, tx_handle)) = self.io_tasks.get_mut().unwrap().take() {
            rx_handle.abort();
            tx_handle.abort();
        }
    }
}

impl GatewayStack {
    /// Create a new userspace NAT stack.
    pub async fn new<F: EthernetFrameIO>(frame_io: F, config: GatewayStackConfig) -> Self {
        let mtu = frame_io.mtu();
        let start_time = std::time::Instant::now();

        let (tx_to_guest, rx_to_guest) = frame_channel(crate::config::channel::DEFAULT);
        let (tx_from_guest, rx_from_guest) = frame_channel(crate::config::channel::DEFAULT);
        let io_task_handles = spawn_frame_io_tasks(frame_io, tx_from_guest, rx_to_guest);

        let mut device = SmoltcpDevice::new(tx_to_guest, mtu);

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

        iface.set_any_ip(true);
        iface
            .routes_mut()
            .add_default_ipv4_route(config.gateway_ip)
            .ok();

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

        let (nat_tx, nat_rx) = frame_channel(crate::config::channel::NAT_RESPONSE);
        let nat = NatTable::new(config.gateway_ip, config.gateway_mac, nat_tx.clone());

        let dns_cache = Arc::new(RwLock::new(DnsCache::new()));
        let dns_proxy = DnsProxy::new(dns_cache.clone()).await;
        let (dns_response_tx, dns_response_rx) = tokio::sync::mpsc::channel(MAX_DNS_QUERIES);
        let dns = DnsDispatcher {
            proxy: dns_proxy,
            cache: dns_cache,
            response_tx: dns_response_tx,
            semaphore: Arc::new(Semaphore::new(MAX_DNS_QUERIES)),
        };

        let tcp_manager = TcpManager::new();
        let (tcp_host_tx, tcp_host_rx) =
            tokio::sync::mpsc::channel(crate::config::channel::TCP_HOST);

        Self {
            device,
            iface,
            sockets,
            dhcp_handle,
            dhcp_server,
            config,
            dns,
            dns_response_rx,
            nat,
            nat_rx,
            tcp_manager,
            tcp_host_rx,
            tcp_host_tx,
            start_time,
            rx_from_guest,
            io_tasks: Mutex::new(Some(io_task_handles)),
        }
    }

    pub(super) fn process_dhcp(&mut self) {
        let socket = self.sockets.get_mut::<udp::Socket>(self.dhcp_handle);

        while let Ok((data, _endpoint)) = socket.recv() {
            if let Ok(dhcp_packet) = DhcpPacket::new_checked(data) {
                let client_mac = dhcp_packet.client_hardware_address();

                match self.dhcp_server.handle_packet(client_mac, &dhcp_packet) {
                    DhcpEvent::Response(response) => {
                        let mut response_buf = vec![0u8; 576];
                        if let Ok(mut response_packet) =
                            DhcpPacket::new_checked(&mut response_buf[..])
                        {
                            if response.emit(&mut response_packet).is_ok() {
                                let dest =
                                    IpEndpoint::new(IpAddress::Ipv4(Ipv4Address::BROADCAST), 68);
                                if let Err(e) = socket.send_slice(&response_buf, dest) {
                                    tracing::warn!("Failed to send DHCP response: {:?}", e);
                                }
                            }
                        }
                    }
                    DhcpEvent::Released(_) | DhcpEvent::None => {}
                }
            }
        }
    }
}
