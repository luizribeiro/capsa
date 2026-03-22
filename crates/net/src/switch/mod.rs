//! Virtual L2 switch for multi-VM communication.
//!
//! This module provides a software switch that allows multiple VMs to
//! communicate with each other on a shared virtual network.

pub mod bridge;
mod port;
pub mod socketpair;

pub use port::*;

use crate::config;
use crate::frame::FrameSender;

use smoltcp::wire::{EthernetAddress, EthernetFrame};
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, Mutex};

/// A virtual L2 switch connecting multiple VMs.
pub struct VirtualSwitch {
    inner: Arc<Mutex<SwitchInner>>,
}

struct SwitchInner {
    /// All connected ports
    ports: Vec<PortHandle>,
    /// MAC address table: MAC → port index
    mac_table: HashMap<EthernetAddress, MacEntry>,
    /// Optional NAT port for external connectivity
    nat_tx: Option<FrameSender>,
}

struct MacEntry {
    port_idx: usize,
}

struct PortHandle {
    id: usize,
    tx: mpsc::Sender<Vec<u8>>,
}

impl VirtualSwitch {
    /// Create a new virtual switch.
    pub fn new() -> Self {
        Self {
            inner: Arc::new(Mutex::new(SwitchInner {
                ports: Vec::new(),
                mac_table: HashMap::new(),
                nat_tx: None,
            })),
        }
    }

    /// Create a new port on this switch.
    /// Returns the port and its guest-side file descriptors (on macOS).
    pub async fn create_port(&self) -> SwitchPort {
        let (to_switch_tx, to_switch_rx) = mpsc::channel(config::channel::SWITCH_PORT);
        let (from_switch_tx, from_switch_rx) = mpsc::channel(config::channel::SWITCH_PORT);

        let port_id = {
            let mut inner = self.inner.lock().await;
            let id = inner.ports.len();
            inner.ports.push(PortHandle {
                id,
                tx: from_switch_tx,
            });
            id
        };

        // Spawn task to handle frames from this port
        let inner = self.inner.clone();
        crate::util::spawn_named(&format!("net-switch-port:{port_id}"), async move {
            Self::port_receiver_task(inner, port_id, to_switch_rx).await;
        });

        SwitchPort {
            id: port_id,
            tx: to_switch_tx,
            rx: tokio::sync::Mutex::new(from_switch_rx),
            pending_frame: std::sync::Mutex::new(None),
        }
    }

    async fn port_receiver_task(
        inner: Arc<Mutex<SwitchInner>>,
        src_port: usize,
        mut rx: mpsc::Receiver<Vec<u8>>,
    ) {
        while let Some(frame) = rx.recv().await {
            let mut switch = inner.lock().await;
            switch.process_frame(src_port, &frame);
        }
    }
}

impl Default for VirtualSwitch {
    fn default() -> Self {
        Self::new()
    }
}

impl SwitchInner {
    fn process_frame(&mut self, src_port: usize, frame: &[u8]) {
        let Ok(eth_frame) = EthernetFrame::new_checked(frame) else {
            return;
        };

        let src_mac = eth_frame.src_addr();
        let dst_mac = eth_frame.dst_addr();

        // Learn source MAC
        self.mac_table
            .insert(src_mac, MacEntry { port_idx: src_port });

        // Forward based on destination MAC
        if dst_mac.is_broadcast() || dst_mac.is_multicast() {
            self.flood(src_port, frame);
        } else if let Some(entry) = self.mac_table.get(&dst_mac) {
            if entry.port_idx != src_port {
                self.send_to_port(entry.port_idx, frame);
            }
        } else {
            self.flood(src_port, frame);
        }
    }

    fn flood(&self, src_port: usize, frame: &[u8]) {
        for port in &self.ports {
            if port.id != src_port {
                Self::try_send(&port.tx, frame.to_vec(), "flood", port.id);
            }
        }
        if let Some(ref nat) = self.nat_tx {
            Self::try_send(nat, frame.to_vec(), "flood NAT", 0);
        }
    }

    fn send_to_port(&self, port_idx: usize, frame: &[u8]) {
        if let Some(port) = self.ports.get(port_idx) {
            Self::try_send(&port.tx, frame.to_vec(), "send", port.id);
        }
    }

    fn try_send(tx: &mpsc::Sender<Vec<u8>>, frame: Vec<u8>, op: &str, port_id: usize) {
        if let Err(e) = tx.try_send(frame) {
            match e {
                mpsc::error::TrySendError::Full(_) => {
                    tracing::debug!(
                        "Switch {} port {}: channel full, dropping frame",
                        op,
                        port_id
                    );
                }
                mpsc::error::TrySendError::Closed(_) => {
                    tracing::warn!("Switch {} port {}: channel closed", op, port_id);
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn create_switch_and_ports() {
        let switch = VirtualSwitch::new();
        let _port1 = switch.create_port().await;
        let _port2 = switch.create_port().await;
    }

    #[tokio::test]
    async fn mac_learning() {
        let switch = VirtualSwitch::new();
        let _port1 = switch.create_port().await;
        let _port2 = switch.create_port().await;

        // Create a simple ethernet frame
        let mut frame = vec![0u8; 64];
        // Dst MAC
        frame[0..6].copy_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]);
        // Src MAC
        frame[6..12].copy_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
        // EtherType (IPv4)
        frame[12..14].copy_from_slice(&[0x08, 0x00]);

        // Simulate frame from port 0
        {
            let mut inner = switch.inner.lock().await;
            inner.process_frame(0, &frame);
        }

        // Check MAC was learned
        {
            let inner = switch.inner.lock().await;
            let mac = EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
            assert!(inner.mac_table.contains_key(&mac));
            assert_eq!(inner.mac_table.get(&mac).unwrap().port_idx, 0);
        }
    }

    #[tokio::test]
    async fn unicast_forwarding_uses_learned_mac_and_does_not_flood() {
        let switch = VirtualSwitch::new();
        let mut port1 = switch.create_port().await;
        let mut port2 = switch.create_port().await;
        let mut port3 = switch.create_port().await;

        let mac1 = [0x02, 0x00, 0x00, 0x00, 0x00, 0x01];
        let mac2 = [0x02, 0x00, 0x00, 0x00, 0x00, 0x02];

        {
            let mut inner = switch.inner.lock().await;
            inner
                .mac_table
                .insert(EthernetAddress(mac1), MacEntry { port_idx: 0 });
            inner
                .mac_table
                .insert(EthernetAddress(mac2), MacEntry { port_idx: 1 });
        }

        let mut unicast = vec![0u8; 64];
        unicast[0..6].copy_from_slice(&mac2);
        unicast[6..12].copy_from_slice(&mac1);
        unicast[12..14].copy_from_slice(&[0x08, 0x00]);
        port1.sender().send(unicast.clone()).await.unwrap();

        let received =
            tokio::time::timeout(std::time::Duration::from_millis(100), port2.recv_frame())
                .await
                .expect("expected unicast frame on destination port")
                .expect("destination port receive failed");
        assert_eq!(received, unicast);

        let leaked =
            tokio::time::timeout(std::time::Duration::from_millis(50), port3.recv_frame()).await;
        assert!(
            leaked.is_err(),
            "unknown port should not receive unicast frame"
        );

        let reflected =
            tokio::time::timeout(std::time::Duration::from_millis(50), port1.recv_frame()).await;
        assert!(
            reflected.is_err(),
            "source port should not receive its own unicast frame"
        );
    }

    #[tokio::test]
    async fn switch_port_handles_burst_traffic() {
        let switch = VirtualSwitch::new();
        let port = switch.create_port().await;
        let sender = port.sender();

        let frame = vec![0u8; 1500];
        for _ in 0..config::channel::SWITCH_PORT {
            sender
                .try_send(frame.clone())
                .expect("channel should accept frames up to capacity");
        }
        assert!(sender.try_send(frame).is_err(), "channel should be full");
    }

    #[tokio::test]
    async fn switch_port_recv_frame_waits_and_returns_frame() {
        let switch = VirtualSwitch::new();
        let port1 = switch.create_port().await;
        let mut port2 = switch.create_port().await;

        // Create a broadcast frame from port1's MAC so it floods to port2
        let mut frame = vec![0u8; 64];
        frame[0..6].copy_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]); // Dst: broadcast
        frame[6..12].copy_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x01]); // Src MAC
        frame[12..14].copy_from_slice(&[0x08, 0x00]); // EtherType

        // Send from port1 to switch (will flood to port2)
        port1.sender().send(frame.clone()).await.unwrap();

        // Give the switch task time to process
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let received = port2.recv_frame().await.expect("recv_frame should succeed");
        assert_eq!(received, frame);
    }

    #[tokio::test]
    async fn switch_port_try_recv_frame_returns_none_when_empty() {
        let switch = VirtualSwitch::new();
        let mut port = switch.create_port().await;

        let result = port.try_recv_frame().expect("try_recv_frame failed");
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn switch_port_try_recv_frame_returns_available_frame() {
        let switch = VirtualSwitch::new();
        let port1 = switch.create_port().await;
        let mut port2 = switch.create_port().await;

        // Create a broadcast frame to flood to port2
        let mut frame = vec![0u8; 64];
        frame[0..6].copy_from_slice(&[0xff, 0xff, 0xff, 0xff, 0xff, 0xff]); // Dst: broadcast
        frame[6..12].copy_from_slice(&[0x02, 0x00, 0x00, 0x00, 0x00, 0x02]); // Src MAC
        frame[12..14].copy_from_slice(&[0x08, 0x00]); // EtherType

        port1.sender().send(frame.clone()).await.unwrap();

        // Give the switch task time to process
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let received = port2
            .try_recv_frame()
            .expect("try_recv_frame failed")
            .expect("expected Some(frame)");
        assert_eq!(received, frame);
    }

    #[tokio::test]
    async fn switch_port_send_frame_completes_for_bulk_sends() {
        let switch = VirtualSwitch::new();
        let mut port = switch.create_port().await;

        let frame = vec![0u8; 64];
        let frame_count = 3000;

        // Verify send_frame() completes successfully for many frames.
        // The underlying mpsc::Sender::send() handles backpressure automatically
        // when the channel fills. We can't easily pause the
        // receiver task to guarantee backpressure, but this test ensures
        // send_frame() doesn't fail or hang under sustained load.
        let result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
            for i in 0..frame_count {
                port.send_frame(&frame)
                    .await
                    .unwrap_or_else(|e| panic!("send_frame failed at frame {}: {}", i, e));
            }
        })
        .await;

        assert!(result.is_ok(), "send_frame timed out - possible hang");
    }
}
