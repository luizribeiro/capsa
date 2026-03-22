mod flow;
mod icmp;
mod udp;

use crate::frame::parse::parse_ipv4_frame;
use crate::frame::FrameSender;
use smoltcp::wire::{EthernetAddress, IpProtocol};
use std::collections::HashMap;
use std::fmt::Debug;
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use icmp::{IcmpKey, IcmpNatEntry, ICMP_IDLE_TIMEOUT};
use udp::{UdpKey, UdpNatEntry, UDP_IDLE_TIMEOUT};

trait NatEntry {
    fn task_handle(&self) -> &JoinHandle<()>;
    fn last_activity(&self) -> Instant;
}

fn cleanup_idle_entries<K: Debug, V: NatEntry>(
    map: &mut HashMap<K, V>,
    now: Instant,
    timeout: Duration,
    label: &str,
) {
    map.retain(|key, entry| {
        if entry.task_handle().is_finished() {
            tracing::warn!("NAT: {label} task for {key:?} finished unexpectedly, removing entry");
            return false;
        }

        let idle_duration = now.duration_since(entry.last_activity());
        if idle_duration > timeout {
            tracing::debug!(
                "NAT: Cleaning up idle {label} entry for {key:?} (idle for {idle_duration:?})"
            );
            entry.task_handle().abort();
            false
        } else {
            true
        }
    });
}

fn abort_all<K, V: NatEntry>(map: &mut HashMap<K, V>) {
    for (_, entry) in map.drain() {
        entry.task_handle().abort();
    }
}

fn spawn_nat_forward_task(
    socket: Arc<UdpSocket>,
    tx: FrameSender,
    cancel: CancellationToken,
    permit: OwnedSemaphorePermit,
    buf_size: usize,
    label: &'static str,
    handler: impl Fn(&[u8], SocketAddr) -> Option<Vec<u8>> + Send + 'static,
) -> JoinHandle<()> {
    crate::util::spawn_named(&format!("nat-{label}-forward"), async move {
        let _permit = permit;
        let mut buf = vec![0u8; buf_size];
        loop {
            tokio::select! {
                biased;

                _ = cancel.cancelled() => {
                    tracing::debug!("NAT: {label} cancelled");
                    break;
                }

                result = socket.recv_from(&mut buf) => {
                    match result {
                        Ok((len, remote_addr)) => {
                            if let Some(frame) = handler(&buf[..len], remote_addr) {
                                if tx.send(frame).await.is_err() {
                                    break;
                                }
                            }
                        }
                        Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                            continue;
                        }
                        Err(e) => {
                            tracing::debug!("NAT: {label} recv error: {e}");
                            break;
                        }
                    }
                }
            }
        }
    })
}

impl NatEntry for UdpNatEntry {
    fn task_handle(&self) -> &JoinHandle<()> {
        &self.task_handle
    }
    fn last_activity(&self) -> Instant {
        self.last_activity
    }
}

impl NatEntry for IcmpNatEntry {
    fn task_handle(&self) -> &JoinHandle<()> {
        &self.task_handle
    }
    fn last_activity(&self) -> Instant {
        self.last_activity
    }
}

const MAX_NAT_TASKS: usize = 512;

pub struct NatTable {
    udp_bindings: HashMap<UdpKey, UdpNatEntry>,
    icmp_bindings: HashMap<IcmpKey, IcmpNatEntry>,
    gateway_ip: Ipv4Addr,
    gateway_mac: EthernetAddress,
    tx_to_guest: FrameSender,
    task_semaphore: Arc<Semaphore>,
    cancellation_token: CancellationToken,
}

impl NatTable {
    pub fn new(gateway_ip: Ipv4Addr, gateway_mac: [u8; 6], tx_to_guest: FrameSender) -> Self {
        Self {
            udp_bindings: HashMap::new(),
            icmp_bindings: HashMap::new(),
            gateway_ip,
            gateway_mac: EthernetAddress(gateway_mac),
            tx_to_guest,
            task_semaphore: Arc::new(Semaphore::new(MAX_NAT_TASKS)),
            cancellation_token: CancellationToken::new(),
        }
    }

    pub async fn process_frame(&mut self, frame: &[u8]) -> bool {
        let Some((eth_frame, ip_packet)) = parse_ipv4_frame(frame) else {
            return false;
        };

        let dst_ip: Ipv4Addr = ip_packet.dst_addr();

        if dst_ip == self.gateway_ip {
            return false;
        }

        let guest_mac = eth_frame.src_addr();
        match ip_packet.next_header() {
            IpProtocol::Udp => self.handle_udp(guest_mac, &ip_packet).await,
            IpProtocol::Icmp => self.handle_icmp(guest_mac, &ip_packet).await,
            _ => false,
        }
    }

    pub fn cleanup(&mut self) {
        let now = Instant::now();
        cleanup_idle_entries(&mut self.udp_bindings, now, UDP_IDLE_TIMEOUT, "UDP");
        cleanup_idle_entries(&mut self.icmp_bindings, now, ICMP_IDLE_TIMEOUT, "ICMP");
    }
}

impl Drop for NatTable {
    fn drop(&mut self) {
        self.cancellation_token.cancel();
        abort_all(&mut self.udp_bindings);
        abort_all(&mut self.icmp_bindings);
    }
}
