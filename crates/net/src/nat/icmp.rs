use crate::frame::craft::craft_icmp_echo_reply;
use smoltcp::phy::ChecksumCapabilities;
use smoltcp::wire::{EthernetAddress, Icmpv4Message, Icmpv4Packet, Icmpv4Repr, Ipv4Packet};
use socket2::{Domain, Protocol, Type};
use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::task::JoinHandle;

use super::flow::{acquire_semaphore, create_flow, NewFlowParams};
use super::NatTable;
use crate::config::ETHERNET_MTU;

pub(crate) const ICMP_IDLE_TIMEOUT: Duration = Duration::from_secs(120);
pub(crate) const MAX_ICMP_BINDINGS_PER_GUEST: usize = 64;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct IcmpKey {
    pub(crate) guest_ip: Ipv4Addr,
    pub(crate) identifier: u16,
    pub(crate) remote_ip: Ipv4Addr,
}

pub(crate) struct IcmpNatEntry {
    pub(crate) socket: Arc<tokio::net::UdpSocket>,
    pub(crate) task_handle: JoinHandle<()>,
    pub(crate) last_activity: Instant,
}

fn create_icmp_socket() -> Option<Arc<tokio::net::UdpSocket>> {
    let socket = match socket2::Socket::new(Domain::IPV4, Type::DGRAM, Some(Protocol::ICMPV4)) {
        Ok(s) => s,
        Err(e) => {
            tracing::warn!("NAT: Failed to create ICMP socket: {}", e);
            return None;
        }
    };
    socket.set_nonblocking(true).ok();

    let std_socket: std::net::UdpSocket = socket.into();
    match tokio::net::UdpSocket::from_std(std_socket) {
        Ok(s) => Some(Arc::new(s)),
        Err(e) => {
            tracing::warn!("NAT: Failed to convert ICMP socket to async: {}", e);
            None
        }
    }
}

impl NatTable {
    pub(crate) async fn handle_icmp(
        &mut self,
        guest_mac: EthernetAddress,
        ip_packet: &Ipv4Packet<&[u8]>,
    ) -> bool {
        let Ok(icmp_packet) = Icmpv4Packet::new_checked(ip_packet.payload()) else {
            return false;
        };
        if icmp_packet.msg_type() != Icmpv4Message::EchoRequest {
            return false;
        }

        let src_ip: Ipv4Addr = ip_packet.src_addr();
        let dst_ip: Ipv4Addr = ip_packet.dst_addr();
        let identifier = icmp_packet.echo_ident();
        let sequence = icmp_packet.echo_seq_no();
        let payload = icmp_packet.data();

        let key = IcmpKey {
            guest_ip: src_ip,
            identifier,
            remote_ip: dst_ip,
        };

        let mut created_new_entry = false;
        if let Some(entry) = self.icmp_bindings.get_mut(&key) {
            entry.last_activity = Instant::now();
        } else {
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

            let Some(socket) = create_icmp_socket() else {
                return false;
            };
            let Some(permit) = acquire_semaphore(&self.task_semaphore, "ICMP", &src_ip) else {
                return false;
            };

            let gateway_mac = self.gateway_mac;
            let guest_ip = src_ip;
            let icmp_id = identifier;
            let expected_remote_ip = dst_ip;

            let (socket, task_handle, last_activity) = create_flow(
                NewFlowParams {
                    socket,
                    permit,
                    buf_size: ETHERNET_MTU,
                    label: "icmp",
                    handler: move |data: &[u8], remote_addr: SocketAddr| {
                        let remote_ip = match remote_addr.ip() {
                            std::net::IpAddr::V4(ip) => ip,
                            _ => return None,
                        };
                        if remote_ip != expected_remote_ip {
                            tracing::debug!(
                                "NAT: ICMP dropping response from {} (expected {})",
                                remote_ip,
                                expected_remote_ip,
                            );
                            return None;
                        }

                        let icmp_data = if data.len() > 20 && (data[0] >> 4) == 4 {
                            let ip_header_len = ((data[0] & 0x0F) as usize) * 4;
                            if data.len() > ip_header_len {
                                &data[ip_header_len..]
                            } else {
                                return None;
                            }
                        } else {
                            data
                        };

                        craft_icmp_echo_reply(
                            remote_ip,
                            guest_ip,
                            icmp_id,
                            icmp_data,
                            gateway_mac,
                            guest_mac,
                        )
                    },
                },
                self.tx_to_guest.clone(),
                self.cancellation_token.clone(),
            );

            self.icmp_bindings.insert(
                key,
                IcmpNatEntry {
                    socket,
                    task_handle,
                    last_activity,
                },
            );
            created_new_entry = true;
        }

        let entry = self.icmp_bindings.get(&key).unwrap();
        let socket = entry.socket.clone();

        let icmp_repr = Icmpv4Repr::EchoRequest {
            ident: identifier,
            seq_no: sequence,
            data: payload,
        };
        let mut icmp_buf = vec![0u8; icmp_repr.buffer_len()];
        let mut icmp_pkt = Icmpv4Packet::new_unchecked(&mut icmp_buf);
        icmp_repr.emit(&mut icmp_pkt, &ChecksumCapabilities::default());

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
                if created_new_entry {
                    if let Some(entry) = self.icmp_bindings.remove(&key) {
                        entry.task_handle.abort();
                    }
                }
                false
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::frame_channel;
    use crate::nat::spawn_nat_forward_task;
    use tokio::net::UdpSocket;
    use tokio::sync::Semaphore;
    use tokio_util::sync::CancellationToken;

    #[tokio::test]
    async fn icmp_response_from_unexpected_ip_is_dropped() {
        // Strategy: set expected_remote_ip to a non-routable IP (192.0.2.1,
        // RFC 5737 TEST-NET). Any packet arriving from 127.0.0.1 will be
        // rejected by the handler since 127.0.0.1 != 192.0.2.1. This avoids
        // needing 127.0.0.2 which is unavailable on macOS.
        let (tx, mut rx) = frame_channel(64);
        let cancel = CancellationToken::new();
        let semaphore = Arc::new(Semaphore::new(1));
        let permit = semaphore.clone().try_acquire_owned().unwrap();

        let socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
        let socket_addr = socket.local_addr().unwrap();

        let expected_remote_ip = Ipv4Addr::new(192, 0, 2, 1); // not 127.0.0.1
        let guest_ip = Ipv4Addr::new(10, 0, 2, 15);
        let icmp_id: u16 = 1234;
        let gateway_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x01]);
        let guest_mac = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);

        let _task = spawn_nat_forward_task(
            socket.clone(),
            tx,
            cancel.clone(),
            permit,
            ETHERNET_MTU,
            "icmp",
            move |data: &[u8], remote_addr: SocketAddr| {
                let remote_ip = match remote_addr.ip() {
                    std::net::IpAddr::V4(ip) => ip,
                    _ => return None,
                };
                if remote_ip != expected_remote_ip {
                    return None;
                }
                craft_icmp_echo_reply(remote_ip, guest_ip, icmp_id, data, gateway_mac, guest_mac)
            },
        );

        // Send from 127.0.0.1 — doesn't match expected 192.0.2.1, should be dropped.
        let sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let mut icmp_reply = vec![0u8; 16];
        icmp_reply[0] = 0; // type = echo reply
        icmp_reply[1] = 0; // code
        icmp_reply[4] = (icmp_id >> 8) as u8;
        icmp_reply[5] = (icmp_id & 0xFF) as u8;
        icmp_reply[6] = 0; // seq_no high
        icmp_reply[7] = 1; // seq_no low
        sender.send_to(&icmp_reply, socket_addr).await.unwrap();

        let result = tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
        assert!(
            result.is_err(),
            "ICMP response from unexpected IP should be dropped"
        );

        cancel.cancel();
    }
}
