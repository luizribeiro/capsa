//! Virtual L2 switch for multi-VM communication.
//!
//! This module provides a software switch that allows multiple VMs to
//! communicate with each other on a shared virtual network.

use crate::frame_io::FrameIO;
use crate::nat::FrameSender;

use smoltcp::wire::{EthernetAddress, EthernetFrame};
use std::collections::HashMap;
use std::io;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::{Duration, Instant};
use tokio::sync::{Mutex, mpsc};

/// MAC address table entry aging time.
const MAC_AGING_SECS: u64 = 300;

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
    last_seen: Instant,
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

    /// Create a new virtual switch with NAT connectivity.
    pub fn with_nat(nat_tx: FrameSender) -> Self {
        Self {
            inner: Arc::new(Mutex::new(SwitchInner {
                ports: Vec::new(),
                mac_table: HashMap::new(),
                nat_tx: Some(nat_tx),
            })),
        }
    }

    /// Create a new port on this switch.
    /// Returns the port and its guest-side file descriptors (on macOS).
    pub async fn create_port(&self) -> SwitchPort {
        let (to_switch_tx, to_switch_rx) = mpsc::channel(256);
        let (from_switch_tx, from_switch_rx) = mpsc::channel(256);

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
        tokio::spawn(async move {
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
            switch.process_frame(src_port, &frame).await;
        }
    }

    /// Run periodic MAC table cleanup.
    pub async fn cleanup(&self) {
        let mut inner = self.inner.lock().await;
        let now = Instant::now();
        inner.mac_table.retain(|_, entry| {
            now.duration_since(entry.last_seen) < Duration::from_secs(MAC_AGING_SECS)
        });
    }
}

impl Default for VirtualSwitch {
    fn default() -> Self {
        Self::new()
    }
}

impl SwitchInner {
    async fn process_frame(&mut self, src_port: usize, frame: &[u8]) {
        let Ok(eth_frame) = EthernetFrame::new_checked(frame) else {
            return;
        };

        let src_mac = eth_frame.src_addr();
        let dst_mac = eth_frame.dst_addr();

        // Learn source MAC
        self.mac_table.insert(
            src_mac,
            MacEntry {
                port_idx: src_port,
                last_seen: Instant::now(),
            },
        );

        // Forward based on destination MAC
        if dst_mac.is_broadcast() || dst_mac.is_multicast() {
            // Flood to all ports except source
            self.flood(src_port, frame).await;
        } else if let Some(entry) = self.mac_table.get(&dst_mac) {
            // Unicast to known destination
            if entry.port_idx != src_port {
                self.send_to_port(entry.port_idx, frame).await;
            }
        } else {
            // Unknown destination, flood
            self.flood(src_port, frame).await;
        }
    }

    async fn flood(&self, src_port: usize, frame: &[u8]) {
        for port in &self.ports {
            if port.id != src_port {
                let _ = port.tx.send(frame.to_vec()).await;
            }
        }
        // Also send to NAT if configured
        if let Some(ref nat) = self.nat_tx {
            let _ = nat.send(frame.to_vec()).await;
        }
    }

    async fn send_to_port(&self, port_idx: usize, frame: &[u8]) {
        if let Some(port) = self.ports.get(port_idx) {
            let _ = port.tx.send(frame.to_vec()).await;
        }
    }
}

/// A port on the virtual switch, implementing FrameIO for VM attachment.
pub struct SwitchPort {
    id: usize,
    tx: mpsc::Sender<Vec<u8>>,
    rx: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
    pending_frame: std::sync::Mutex<Option<Vec<u8>>>,
}

impl SwitchPort {
    /// Get the port ID.
    pub fn id(&self) -> usize {
        self.id
    }
}

impl FrameIO for SwitchPort {
    fn mtu(&self) -> usize {
        1500
    }

    fn poll_recv(&mut self, _cx: &mut Context<'_>, buf: &mut [u8]) -> Poll<io::Result<usize>> {
        // Try to get from pending first
        {
            let mut pending = self.pending_frame.lock().unwrap();
            if let Some(frame) = pending.take() {
                let len = frame.len().min(buf.len());
                buf[..len].copy_from_slice(&frame[..len]);
                return Poll::Ready(Ok(len));
            }
        }

        // Try non-blocking receive
        let mut rx = match self.rx.try_lock() {
            Ok(rx) => rx,
            Err(_) => return Poll::Pending,
        };

        match rx.try_recv() {
            Ok(frame) => {
                let len = frame.len().min(buf.len());
                buf[..len].copy_from_slice(&frame[..len]);
                Poll::Ready(Ok(len))
            }
            Err(mpsc::error::TryRecvError::Empty) => Poll::Pending,
            Err(mpsc::error::TryRecvError::Disconnected) => Poll::Ready(Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "switch closed",
            ))),
        }
    }

    fn send(&mut self, frame: &[u8]) -> io::Result<()> {
        self.tx
            .try_send(frame.to_vec())
            .map_err(|e| io::Error::new(io::ErrorKind::WouldBlock, e))
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
            inner.process_frame(0, &frame).await;
        }

        // Check MAC was learned
        {
            let inner = switch.inner.lock().await;
            let mac = EthernetAddress([0x02, 0x00, 0x00, 0x00, 0x00, 0x01]);
            assert!(inner.mac_table.contains_key(&mac));
            assert_eq!(inner.mac_table.get(&mac).unwrap().port_idx, 0);
        }
    }
}
