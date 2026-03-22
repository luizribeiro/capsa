use crate::frame::craft::craft_udp_response;
use smoltcp::wire::{EthernetAddress, Ipv4Packet, UdpPacket};
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

use super::flow::{acquire_semaphore, create_flow, NewFlowParams};
use super::NatTable;

#[cfg(test)]
use smoltcp::phy::ChecksumCapabilities;
#[cfg(test)]
use smoltcp::wire::{EthernetFrame, EthernetProtocol, EthernetRepr, IpProtocol, Ipv4Repr, UdpRepr};

pub(crate) const UDP_IDLE_TIMEOUT: Duration = Duration::from_secs(60);
pub(crate) const MAX_UDP_BINDINGS: usize = 256;
const UDP_RECV_BUF: usize = 4096;

#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
pub(crate) struct UdpKey {
    pub(crate) guest_addr: SocketAddrV4,
    pub(crate) remote_ip: Ipv4Addr,
}

pub(crate) struct UdpNatEntry {
    pub(crate) socket: Arc<UdpSocket>,
    pub(crate) task_handle: JoinHandle<()>,
    pub(crate) last_activity: Instant,
}

impl NatTable {
    pub(crate) async fn handle_udp(
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
        let key = UdpKey {
            guest_addr: src,
            remote_ip: dst_ip,
        };

        let (socket, created_new_entry) = if let Some(entry) = self.udp_bindings.get_mut(&key) {
            entry.last_activity = Instant::now();
            (entry.socket.clone(), false)
        } else {
            if self.udp_bindings.len() >= MAX_UDP_BINDINGS {
                tracing::warn!(
                    "NAT: UDP binding limit reached ({}), rejecting {}",
                    MAX_UDP_BINDINGS,
                    src
                );
                return false;
            }

            let Some(permit) = acquire_semaphore(&self.task_semaphore, "UDP", &src) else {
                return false;
            };

            let socket = match UdpSocket::bind("0.0.0.0:0").await {
                Ok(s) => Arc::new(s),
                Err(e) => {
                    tracing::warn!("NAT: UDP bind failed: {}", e);
                    return false;
                }
            };

            let gateway_mac = self.gateway_mac;
            let guest_addr = src;
            let expected_remote_ip = dst_ip;

            let (socket, task_handle, last_activity) = create_flow(
                NewFlowParams {
                    socket,
                    permit,
                    buf_size: UDP_RECV_BUF,
                    label: "udp",
                    handler: move |data: &[u8], remote_addr: SocketAddr| {
                        let remote = match remote_addr {
                            SocketAddr::V4(v4) => v4,
                            SocketAddr::V6(_) => return None,
                        };
                        if *remote.ip() != expected_remote_ip {
                            tracing::debug!(
                                "NAT: UDP dropping response from {} (expected {})",
                                remote.ip(),
                                expected_remote_ip,
                            );
                            return None;
                        }
                        Some(craft_udp_response(
                            data,
                            remote,
                            guest_addr,
                            gateway_mac,
                            guest_mac,
                        ))
                    },
                },
                self.tx_to_guest.clone(),
                self.cancellation_token.clone(),
            );

            self.udp_bindings.insert(
                key,
                UdpNatEntry {
                    socket: socket.clone(),
                    task_handle,
                    last_activity,
                },
            );

            (socket, true)
        };

        let payload = udp_packet.payload();
        match socket.send_to(payload, SocketAddr::V4(dst)).await {
            Ok(_) => {
                tracing::debug!("NAT: UDP {} -> {} ({} bytes)", src, dst, payload.len());
                true
            }
            Err(e) => {
                tracing::warn!("NAT: UDP send to {} failed: {}", dst, e);
                if created_new_entry {
                    if let Some(entry) = self.udp_bindings.remove(&key) {
                        entry.task_handle.abort();
                    }
                }
                false
            }
        }
    }
}

#[cfg(test)]
fn craft_udp_frame(
    src_mac: EthernetAddress,
    dst_mac: EthernetAddress,
    src_ip: Ipv4Addr,
    dst_ip: Ipv4Addr,
    src_port: u16,
    dst_port: u16,
    payload: &[u8],
) -> Vec<u8> {
    let udp_repr = UdpRepr { src_port, dst_port };
    let udp_len = udp_repr.header_len() + payload.len();
    let ip_repr = Ipv4Repr {
        src_addr: src_ip,
        dst_addr: dst_ip,
        next_header: IpProtocol::Udp,
        payload_len: udp_len,
        hop_limit: 64,
    };
    let ip_len = ip_repr.buffer_len() + udp_len;
    let total_len = EthernetFrame::<&[u8]>::header_len() + ip_len;

    let mut frame = vec![0u8; total_len];
    let eth_repr = EthernetRepr {
        src_addr: src_mac,
        dst_addr: dst_mac,
        ethertype: EthernetProtocol::Ipv4,
    };
    let mut eth = EthernetFrame::new_unchecked(&mut frame);
    eth_repr.emit(&mut eth);

    let mut ip_pkt = Ipv4Packet::new_unchecked(&mut frame[EthernetFrame::<&[u8]>::header_len()..]);
    ip_repr.emit(&mut ip_pkt, &ChecksumCapabilities::default());

    let ip_header_len = ip_repr.buffer_len();
    let udp_start = EthernetFrame::<&[u8]>::header_len() + ip_header_len;
    let mut udp_pkt = UdpPacket::new_unchecked(&mut frame[udp_start..]);
    udp_repr.emit(
        &mut udp_pkt,
        &src_ip.into(),
        &dst_ip.into(),
        payload.len(),
        |buf| buf.copy_from_slice(payload),
        &ChecksumCapabilities::default(),
    );

    frame
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::frame_channel;

    const GATEWAY_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 1);
    const GATEWAY_MAC: [u8; 6] = [0x52, 0x54, 0x00, 0x00, 0x00, 0x01];
    const GUEST_MAC: EthernetAddress = EthernetAddress([0x52, 0x54, 0x00, 0x00, 0x00, 0x02]);
    const GUEST_IP: Ipv4Addr = Ipv4Addr::new(10, 0, 2, 15);
    const REMOTE_IP_A: Ipv4Addr = Ipv4Addr::new(93, 184, 216, 34);
    const REMOTE_IP_B: Ipv4Addr = Ipv4Addr::new(198, 51, 100, 1);

    fn make_nat(tx: crate::frame::FrameSender) -> NatTable {
        NatTable::new(GATEWAY_IP, GATEWAY_MAC, tx)
    }

    #[tokio::test]
    async fn separate_bindings_per_remote_ip() {
        let (tx, _rx) = frame_channel(64);
        let mut nat = make_nat(tx);

        let frame_a = craft_udp_frame(
            GUEST_MAC,
            EthernetAddress(GATEWAY_MAC),
            GUEST_IP,
            REMOTE_IP_A,
            5000,
            80,
            b"hello A",
        );
        let frame_b = craft_udp_frame(
            GUEST_MAC,
            EthernetAddress(GATEWAY_MAC),
            GUEST_IP,
            REMOTE_IP_B,
            5000,
            80,
            b"hello B",
        );

        assert!(nat.process_frame(&frame_a).await);
        assert!(nat.process_frame(&frame_b).await);

        assert_eq!(nat.udp_bindings.len(), 2);
        let key_a = UdpKey {
            guest_addr: SocketAddrV4::new(GUEST_IP, 5000),
            remote_ip: REMOTE_IP_A,
        };
        let key_b = UdpKey {
            guest_addr: SocketAddrV4::new(GUEST_IP, 5000),
            remote_ip: REMOTE_IP_B,
        };
        assert!(nat.udp_bindings.contains_key(&key_a));
        assert!(nat.udp_bindings.contains_key(&key_b));
    }

    #[tokio::test]
    async fn same_remote_reuses_binding() {
        let (tx, _rx) = frame_channel(64);
        let mut nat = make_nat(tx);

        let frame = craft_udp_frame(
            GUEST_MAC,
            EthernetAddress(GATEWAY_MAC),
            GUEST_IP,
            REMOTE_IP_A,
            5000,
            80,
            b"hello",
        );

        assert!(nat.process_frame(&frame).await);
        assert!(nat.process_frame(&frame).await);

        assert_eq!(nat.udp_bindings.len(), 1);
    }

    #[tokio::test]
    async fn response_from_wrong_remote_ip_is_dropped() {
        // Strategy: create a NAT binding that expects responses from a
        // non-routable IP (192.0.2.1, RFC 5737 TEST-NET). The NAT socket
        // binds to 0.0.0.0 so we can send to it from 127.0.0.1 — but the
        // handler will see remote_ip=127.0.0.1 != expected=192.0.2.1 and
        // drop it. This avoids needing 127.0.0.2 (unavailable on macOS).
        let (tx, mut rx) = frame_channel(64);
        let mut nat = make_nat(tx);

        // Use a non-routable IP as the "expected" remote. The send_to will
        // fail, but we don't care — we just need the binding created so the
        // forward task is listening on the NAT socket.
        let fake_remote = Ipv4Addr::new(192, 0, 2, 1);
        let frame = craft_udp_frame(
            GUEST_MAC,
            EthernetAddress(GATEWAY_MAC),
            GUEST_IP,
            fake_remote,
            5000,
            80,
            b"ping",
        );
        // process_frame sends to 192.0.2.1:80 which will fail, but the
        // binding and forward task are still created.
        let _ = nat.process_frame(&frame).await;

        let key = UdpKey {
            guest_addr: SocketAddrV4::new(GUEST_IP, 5000),
            remote_ip: fake_remote,
        };
        let nat_socket = nat.udp_bindings.get(&key).unwrap().socket.clone();
        let nat_local_addr = nat_socket.local_addr().unwrap();
        let nat_send_addr = SocketAddr::new(
            std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
            nat_local_addr.port(),
        );

        // Send from 127.0.0.1 to the NAT socket — the handler expects
        // 192.0.2.1 so this should be silently dropped.
        let spoofed_sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        spoofed_sender
            .send_to(b"spoofed", nat_send_addr)
            .await
            .unwrap();

        let result: Result<Option<Vec<u8>>, _> =
            tokio::time::timeout(Duration::from_millis(200), rx.recv()).await;
        assert!(result.is_err(), "spoofed packet should not arrive on rx");
    }

    #[tokio::test]
    async fn response_from_same_ip_different_port_is_forwarded() {
        let (tx, mut rx) = frame_channel(64);
        let mut nat = make_nat(tx);

        let echo_server = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let echo_addr = echo_server.local_addr().unwrap();

        let frame = craft_udp_frame(
            GUEST_MAC,
            EthernetAddress(GATEWAY_MAC),
            GUEST_IP,
            Ipv4Addr::LOCALHOST,
            5000,
            echo_addr.port(),
            b"ping",
        );
        assert!(nat.process_frame(&frame).await);

        let key = UdpKey {
            guest_addr: SocketAddrV4::new(GUEST_IP, 5000),
            remote_ip: Ipv4Addr::LOCALHOST,
        };
        let nat_socket = nat.udp_bindings.get(&key).unwrap().socket.clone();
        let nat_local_addr = nat_socket.local_addr().unwrap();
        // The NAT socket binds to 0.0.0.0, which isn't routable on macOS.
        // Use 127.0.0.1 with the same port for sending.
        let nat_send_addr = SocketAddr::new(
            std::net::IpAddr::V4(Ipv4Addr::LOCALHOST),
            nat_local_addr.port(),
        );

        let mut buf = [0u8; 64];
        let (_len, _from) = echo_server.recv_from(&mut buf).await.unwrap();

        // Send from same IP (127.0.0.1) but a different port — address-restricted
        // cone NAT allows this (only IP is validated, not port).
        let other_port_sender = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        other_port_sender
            .send_to(b"different port", nat_send_addr)
            .await
            .unwrap();

        let frame: Vec<u8> = tokio::time::timeout(Duration::from_secs(2), rx.recv())
            .await
            .expect("timed out — packet from same IP different port should be forwarded")
            .expect("channel closed");
        let eth = EthernetFrame::new_checked(&frame).unwrap();
        let ip = Ipv4Packet::new_checked(eth.payload()).unwrap();
        let udp = UdpPacket::new_checked(ip.payload()).unwrap();
        assert_eq!(udp.payload(), b"different port");
    }

    #[tokio::test]
    async fn binding_limit_caps_total_entries() {
        let (tx, _rx) = frame_channel(64);
        let mut nat = make_nat(tx);

        for i in 0..MAX_UDP_BINDINGS {
            let remote_ip = Ipv4Addr::new(10, 1, (i / 256) as u8, (i % 256) as u8);
            let frame = craft_udp_frame(
                GUEST_MAC,
                EthernetAddress(GATEWAY_MAC),
                GUEST_IP,
                remote_ip,
                5000,
                80,
                b"x",
            );
            assert!(
                nat.process_frame(&frame).await,
                "binding {i} should succeed"
            );
        }

        assert_eq!(nat.udp_bindings.len(), MAX_UDP_BINDINGS);

        let frame = craft_udp_frame(
            GUEST_MAC,
            EthernetAddress(GATEWAY_MAC),
            GUEST_IP,
            Ipv4Addr::new(10, 2, 0, 0),
            5000,
            80,
            b"x",
        );
        assert!(
            !nat.process_frame(&frame).await,
            "should reject when at binding limit"
        );
    }
}
