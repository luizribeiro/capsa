use super::bounded_queue::BoundedQueue;
use crate::config;
use crate::frame::FrameSender;
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;

/// Shared pending TX queue for smoltcp frames.
/// Wrapped in Arc<Mutex> so TxToken can queue frames without &mut self.
type PendingTxQueue = Arc<Mutex<BoundedQueue<Vec<u8>>>>;

/// Wraps frame channels to implement smoltcp's Device trait.
///
/// Unlike the previous implementation that owned `EthernetFrameIO`, this version
/// uses channels for frame I/O. This allows the main loop to use `tokio::select!`
/// for event-driven I/O while SmoltcpDevice handles smoltcp's synchronous Device trait.
///
/// Smoltcp-generated frames (ARP, DHCP, ICMP responses) are queued in `pending_tx`
/// rather than dropped when the channel is full. The main loop drains this queue
/// asynchronously via `drain_pending_tx()`.
pub struct SmoltcpDevice {
    /// Pending frame for smoltcp to consume.
    rx_buffer: Option<Vec<u8>>,
    /// Channel for sending frames to guest (used by TxToken).
    tx_to_guest: FrameSender,
    /// Pending TX frames from smoltcp (buffered when channel is full).
    pending_tx: PendingTxQueue,
    /// MTU for this device.
    mtu: usize,
}

impl SmoltcpDevice {
    /// Create a new SmoltcpDevice with the given transmit channel and MTU.
    pub fn new(tx_to_guest: FrameSender, mtu: usize) -> Self {
        Self {
            rx_buffer: None,
            tx_to_guest,
            pending_tx: Arc::new(Mutex::new(BoundedQueue::new(config::pending::SMOLTCP_TX))),
            mtu,
        }
    }

    /// Queue a received frame for smoltcp to process.
    ///
    /// Call this when a frame arrives that should be handled by smoltcp
    /// (ARP, ICMP, DHCP, etc.). The frame will be consumed by the next
    /// `iface.poll()` call.
    pub fn queue_rx_frame(&mut self, frame: Vec<u8>) {
        debug_assert!(
            self.rx_buffer.is_none(),
            "queue_rx_frame called with pending frame"
        );
        self.rx_buffer = Some(frame);
    }

    #[cfg(test)]
    pub fn has_pending_rx(&self) -> bool {
        self.rx_buffer.is_some()
    }

    #[cfg(test)]
    pub fn has_pending_tx(&self) -> bool {
        !self.pending_tx.lock().unwrap().is_empty()
    }

    /// Take the next pending smoltcp TX frame, if any.
    pub fn take_pending_tx(&self) -> Option<Vec<u8>> {
        self.pending_tx.lock().unwrap().pop()
    }

    /// Get a clone of the transmit channel sender.
    ///
    /// Useful for passing to other components that need to send frames.
    pub fn tx_sender(&self) -> FrameSender {
        self.tx_to_guest.clone()
    }
}

impl Device for SmoltcpDevice {
    type RxToken<'a>
        = SmoltcpRxToken
    where
        Self: 'a;
    type TxToken<'a>
        = SmoltcpTxToken
    where
        Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = self.mtu;
        caps.medium = Medium::Ethernet;
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let frame = self.rx_buffer.take()?;

        let rx_token = SmoltcpRxToken { frame };
        let tx_token = SmoltcpTxToken {
            tx: self.tx_to_guest.clone(),
            pending: self.pending_tx.clone(),
        };

        Some((rx_token, tx_token))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        if self.tx_to_guest.capacity() == 0 && !self.pending_tx.lock().unwrap().has_room() {
            return None;
        }
        Some(SmoltcpTxToken {
            tx: self.tx_to_guest.clone(),
            pending: self.pending_tx.clone(),
        })
    }
}

/// Receive token for smoltcp.
pub struct SmoltcpRxToken {
    frame: Vec<u8>,
}

impl RxToken for SmoltcpRxToken {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(&self.frame)
    }
}

/// Transmit token for smoltcp.
///
/// Queues frames to `pending` when the channel is full, rather than dropping.
/// The main loop drains pending frames asynchronously.
pub struct SmoltcpTxToken {
    tx: FrameSender,
    pending: PendingTxQueue,
}

impl TxToken for SmoltcpTxToken {
    fn consume<R, Func>(self, len: usize, f: Func) -> R
    where
        Func: FnOnce(&mut [u8]) -> R,
    {
        let mut buf = vec![0u8; len];
        let result = f(&mut buf);

        // Try to send directly first
        match self.tx.try_send(buf) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(frame)) => {
                // Channel full - queue for async drain instead of dropping.
                // This preserves ARP/DHCP frames that would otherwise be lost.
                let mut pending = self.pending.lock().unwrap();
                if pending.push(frame).is_err() {
                    tracing::warn!(
                        "Smoltcp pending queue full ({} frames), dropping frame",
                        pending.capacity()
                    );
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                tracing::warn!("Frame send error: channel closed");
            }
        }
        result
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::frame_channel;

    #[test]
    fn test_queue_and_consume_rx_frame() {
        let (tx, _rx) = frame_channel(config::channel::DEFAULT);
        let mut device = SmoltcpDevice::new(tx, 1500);

        assert!(!device.has_pending_rx());

        device.queue_rx_frame(vec![1, 2, 3, 4]);
        assert!(device.has_pending_rx());

        // Consume via Device trait
        let timestamp = Instant::from_millis(0);
        let (rx_token, _tx_token) = device.receive(timestamp).expect("should have frame");

        let data = rx_token.consume(|buf| buf.to_vec());
        assert_eq!(data, vec![1, 2, 3, 4]);

        assert!(!device.has_pending_rx());
    }

    #[test]
    fn test_transmit_sends_to_channel() {
        let (tx, mut rx) = frame_channel(config::channel::DEFAULT);
        let mut device = SmoltcpDevice::new(tx, 1500);

        let timestamp = Instant::from_millis(0);
        let tx_token = device.transmit(timestamp).expect("should get tx token");

        tx_token.consume(5, |buf| {
            buf.copy_from_slice(&[10, 20, 30, 40, 50]);
        });

        let frame = rx.try_recv().expect("should have frame in channel");
        assert_eq!(frame, vec![10, 20, 30, 40, 50]);
    }

    #[test]
    fn transmit_returns_none_when_fully_congested() {
        let (tx, _rx) = frame_channel(1);
        let mut device = SmoltcpDevice::new(tx, 1500);
        let ts = Instant::from_millis(0);

        // Fill the channel (capacity 1)
        let token = device.transmit(ts).unwrap();
        token.consume(4, |buf| buf.copy_from_slice(&[1, 2, 3, 4]));

        // Channel full but pending_tx has room — should still return Some
        assert!(device.transmit(ts).is_some());

        // Fill pending_tx until no room left
        loop {
            if !device.pending_tx.lock().unwrap().has_room() {
                break;
            }
            let token = device
                .transmit(ts)
                .expect("should return Some while pending_tx has room");
            token.consume(1, |buf| buf[0] = 0);
        }

        // Both full — transmit must return None (backpressure)
        assert!(device.transmit(ts).is_none());
    }

    #[test]
    #[should_panic(expected = "queue_rx_frame called with pending frame")]
    fn queue_rx_frame_overwrites_panics_in_debug() {
        let (tx, _rx) = frame_channel(config::channel::DEFAULT);
        let mut device = SmoltcpDevice::new(tx, 1500);

        device.queue_rx_frame(vec![1, 2, 3]);
        device.queue_rx_frame(vec![4, 5, 6]);
    }

    #[test]
    fn queue_rx_frame_after_consume_succeeds() {
        let (tx, _rx) = frame_channel(config::channel::DEFAULT);
        let mut device = SmoltcpDevice::new(tx, 1500);

        device.queue_rx_frame(vec![1, 2, 3]);

        let timestamp = Instant::from_millis(0);
        device.receive(timestamp).expect("should have frame");

        device.queue_rx_frame(vec![4, 5, 6]);
        assert!(device.has_pending_rx());
    }

    #[test]
    fn test_tx_token_queues_when_channel_full() {
        // Create a channel with capacity 1
        let (tx, mut rx) = frame_channel(1);
        let mut device = SmoltcpDevice::new(tx, 1500);

        // First transmit should go to channel
        let tx_token = device.transmit(Instant::from_millis(0)).unwrap();
        tx_token.consume(4, |buf| buf.copy_from_slice(&[1, 2, 3, 4]));

        // Channel should have the frame
        let frame = rx.try_recv().expect("should have first frame");
        assert_eq!(frame, vec![1, 2, 3, 4]);

        // Fill the channel again
        let tx_token = device.transmit(Instant::from_millis(0)).unwrap();
        tx_token.consume(4, |buf| buf.copy_from_slice(&[5, 6, 7, 8]));

        // Second transmit while channel is full should queue to pending
        let tx_token = device.transmit(Instant::from_millis(0)).unwrap();
        tx_token.consume(4, |buf| buf.copy_from_slice(&[9, 10, 11, 12]));

        // Check pending queue has the frame
        assert!(device.has_pending_tx());
        let pending = device.take_pending_tx().expect("should have pending frame");
        assert_eq!(pending, vec![9, 10, 11, 12]);

        // Pending queue should now be empty
        assert!(!device.has_pending_tx());
    }
}
