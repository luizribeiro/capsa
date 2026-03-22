use crate::frame::craft::craft_udp_response;
use crate::frame::smoltcp_now;

use super::config::{CLEANUP_INTERVAL, DHCP_LEASE_TIMEOUT};
use super::outbound_buffer::OutboundFrameBuffer;
use super::GatewayStack;

use std::net::SocketAddrV4;

use smoltcp::iface::PollResult;
use smoltcp::wire::EthernetAddress;

impl GatewayStack {
    /// Run the network stack.
    pub async fn run(mut self) -> Result<(), std::io::Error> {
        let mut cleanup_interval = tokio::time::interval(CLEANUP_INTERVAL);

        let tx_to_guest = self.device.tx_sender();
        let mut outbound = OutboundFrameBuffer::new(
            tx_to_guest.clone(),
            crate::config::pending::TO_GUEST,
            crate::config::flow::SEND_BUDGET,
        );

        loop {
            while outbound.has_room() {
                match self.device.take_pending_tx() {
                    Some(frame) => {
                        let _ = outbound.push(frame);
                    }
                    None => break,
                }
            }

            outbound.flush();

            let timestamp = smoltcp_now(self.start_time);
            let smoltcp_delay: std::time::Duration = self
                .iface
                .poll_delay(timestamp, &self.sockets)
                .map(Into::into)
                .unwrap_or(std::time::Duration::from_millis(100));

            tokio::select! {
                Some(frame) = self.rx_from_guest.recv() => {
                    self.handle_guest_frame(frame, &mut outbound).await;
                    for _ in 1..crate::config::flow::GUEST_FRAME_DRAIN_BUDGET {
                        match self.rx_from_guest.try_recv() {
                            Ok(frame) => {
                                let ts = smoltcp_now(self.start_time);
                                self.iface.poll(ts, &mut self.device, &mut self.sockets);
                                self.handle_guest_frame(frame, &mut outbound).await;
                            }
                            Err(_) => break,
                        }
                    }
                }

                Some(frame) = self.nat_rx.recv(), if outbound.has_room() => {
                    let _ = outbound.push(frame);
                }

                Some(dns_resp) = self.dns_response_rx.recv(), if outbound.has_room() => {
                    let src = SocketAddrV4::new(self.config.gateway_ip, 53);
                    let dst = SocketAddrV4::new(dns_resp.guest_ip, dns_resp.guest_port);
                    let gateway_mac = EthernetAddress(self.config.gateway_mac);
                    let frame = craft_udp_response(
                        &dns_resp.response_bytes,
                        src,
                        dst,
                        gateway_mac,
                        dns_resp.guest_mac,
                    );
                    outbound.push_logged(frame, "DNS");
                }

                Some(event) = self.tcp_host_rx.recv() => {
                    self.tcp_manager.handle_host_event(event, &mut self.sockets);
                    for _ in 1..crate::config::flow::TCP_HOST_DRAIN_BUDGET {
                        match self.tcp_host_rx.try_recv() {
                            Ok(event) => {
                                let ts = smoltcp_now(self.start_time);
                                self.iface.poll(ts, &mut self.device, &mut self.sockets);
                                self.tcp_manager.handle_host_event(event, &mut self.sockets);
                            }
                            Err(_) => break,
                        }
                    }
                }

                Ok(permit) = tx_to_guest.reserve(), if outbound.pending_len() > 0 => {
                    if let Some(frame) = outbound.pop_next() {
                        permit.send(frame);
                    }
                }

                _ = tokio::time::sleep(smoltcp_delay) => {}

                _ = cleanup_interval.tick() => {
                    self.nat.cleanup();
                    self.dns.cleanup_cache();
                    self.tcp_manager.cleanup(&mut self.sockets);
                    let _ = self.dhcp_server.cleanup_expired(DHCP_LEASE_TIMEOUT);
                }
            }

            let timestamp = smoltcp_now(self.start_time);
            self.iface
                .poll(timestamp, &mut self.device, &mut self.sockets);
            self.process_dhcp();

            self.tcp_manager.poll_connect_results(&mut self.sockets);
            self.tcp_manager
                .poll_newly_established(&mut self.sockets, &self.tcp_host_tx);
            self.tcp_manager.poll_sockets(&mut self.sockets);

            for _ in 0..crate::config::flow::EGRESS_POLL_BUDGET {
                let ts = smoltcp_now(self.start_time);
                if self
                    .iface
                    .poll_egress(ts, &mut self.device, &mut self.sockets)
                    == PollResult::None
                {
                    break;
                }
            }
        }
    }
}
