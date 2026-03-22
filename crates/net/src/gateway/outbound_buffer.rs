//! Outbound frame buffer for budgeted frame delivery.
//!
//! This module provides `OutboundFrameBuffer`, which composes a `BoundedQueue` with a
//! `FrameSender` to provide budgeted flush semantics. It's designed for `GatewayStack`
//! where we need to:
//!
//! 1. Buffer outbound frames when the channel is full
//! 2. Flush a limited number of frames per iteration (to prevent starvation)
//! 3. Provide backpressure signaling via `has_room()`
//!
//! ## Drop Semantics (Load-Bearing)
//!
//! - **Smoltcp frames**: `push()` may return `Err`; caller can discard (best-effort)
//! - **NAT frames**: Must not drop; caller gates on `has_room()` before receiving
//!
//! The caller is responsible for backpressure by gating the `nat_rx` select branch:
//!
//! ```ignore
//! tokio::select! {
//!     Some(frame) = rx_from_guest.recv() => { /* always accept */ }
//!     Some(frame) = nat_rx.recv(), if buffer.has_room() => {
//!         buffer.push(frame).expect("has_room was true");
//!     }
//! }
//! ```

use super::bounded_queue::BoundedQueue;
use crate::frame::FrameSender;

/// A buffer for outbound frames with budgeted flush semantics.
///
/// Composes a `BoundedQueue` for overflow buffering with a `FrameSender` for delivery.
/// Flushes are budget-limited to prevent any single consumer from starving others.
pub struct OutboundFrameBuffer {
    pending: BoundedQueue<Vec<u8>>,
    sender: FrameSender,
    budget_per_flush: usize,
    /// Frame that couldn't be sent due to full channel, preserved for next flush.
    /// This ensures FIFO ordering is maintained when the channel fills up.
    deferred: Option<Vec<u8>>,
}

impl OutboundFrameBuffer {
    /// Creates a new outbound frame buffer.
    ///
    /// # Arguments
    ///
    /// * `sender` - The channel to send frames to
    /// * `pending_capacity` - Maximum frames to buffer when channel is full
    /// * `budget` - Maximum frames to send per flush
    pub fn new(sender: FrameSender, pending_capacity: usize, budget: usize) -> Self {
        Self {
            pending: BoundedQueue::new(pending_capacity),
            sender,
            budget_per_flush: budget,
            deferred: None,
        }
    }

    /// Queues a frame for sending.
    ///
    /// Returns `Err(frame)` if the pending queue is full.
    ///
    /// # Important
    ///
    /// Callers must handle the `Err` case appropriately:
    /// - Smoltcp frames (ARP/DHCP): Can be dropped (best-effort)
    /// - NAT response frames: Must NOT be dropped — use `has_room()` to gate
    ///   the select! branch that receives from `nat_rx`
    pub fn push(&mut self, frame: Vec<u8>) -> Result<(), Vec<u8>> {
        self.pending.push(frame)
    }

    /// Flushes up to `budget_per_flush` frames to the channel.
    ///
    /// Returns early if the channel is full (preserves remaining frames in pending queue).
    pub fn flush(&mut self) {
        let mut sent = 0;

        while sent < self.budget_per_flush {
            let frame = self.deferred.take().or_else(|| self.pending.pop());

            let Some(frame) = frame else {
                break;
            };

            match self.sender.try_send(frame) {
                Ok(()) => {
                    sent += 1;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Full(frame)) => {
                    tracing::debug!(
                        sent = sent,
                        pending = self.pending.len(),
                        "OutboundFrameBuffer flush: channel full, deferring frame"
                    );
                    self.deferred = Some(frame);
                    break;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    break;
                }
            }
        }
    }

    /// Returns true if the pending queue has room for more frames.
    ///
    /// Use this to gate `select!` branches that should apply backpressure
    /// when the buffer is full.
    pub fn has_room(&self) -> bool {
        self.pending.has_room()
    }

    pub(super) fn push_logged(&mut self, frame: Vec<u8>, frame_type: &str) -> bool {
        match self.push(frame) {
            Ok(()) => true,
            Err(_dropped_frame) => {
                tracing::warn!(
                    "Dropping {} frame due to backpressure (queue at {} frames)",
                    frame_type,
                    crate::config::pending::TO_GUEST
                );
                false
            }
        }
    }

    /// Returns the total number of pending frames (including any deferred frame).
    pub fn pending_len(&self) -> usize {
        self.pending.len() + usize::from(self.deferred.is_some())
    }

    /// Pops the next frame to send (deferred first, then pending).
    ///
    /// Use this when you have a channel permit and want to send exactly one frame.
    /// This maintains FIFO ordering by prioritizing the deferred frame.
    pub fn pop_next(&mut self) -> Option<Vec<u8>> {
        self.deferred.take().or_else(|| self.pending.pop())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::frame::frame_channel;

    #[tokio::test]
    async fn push_succeeds_when_room_available() {
        let (tx, _rx) = frame_channel(16);
        let mut buffer = OutboundFrameBuffer::new(tx, 4, 2);

        assert!(buffer.push(vec![0x01]).is_ok());
        assert!(buffer.push(vec![0x02]).is_ok());
        assert!(buffer.push(vec![0x03]).is_ok());
        assert!(buffer.push(vec![0x04]).is_ok());

        assert_eq!(buffer.pending_len(), 4);
    }

    #[tokio::test]
    async fn push_returns_err_when_full() {
        let (tx, _rx) = frame_channel(16);
        let mut buffer = OutboundFrameBuffer::new(tx, 2, 2);

        assert!(buffer.push(vec![0x01]).is_ok());
        assert!(buffer.push(vec![0x02]).is_ok());

        let result = buffer.push(vec![0x03]);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err(), vec![0x03]);
        assert_eq!(buffer.pending_len(), 2);
    }

    #[tokio::test]
    async fn flush_sends_up_to_budget_frames() {
        let (tx, mut rx) = frame_channel(16);
        let mut buffer = OutboundFrameBuffer::new(tx, 8, 3);

        buffer.push(vec![0x01]).unwrap();
        buffer.push(vec![0x02]).unwrap();
        buffer.push(vec![0x03]).unwrap();
        buffer.push(vec![0x04]).unwrap();
        buffer.push(vec![0x05]).unwrap();

        buffer.flush();

        assert_eq!(buffer.pending_len(), 2);

        assert_eq!(rx.recv().await.unwrap(), vec![0x01]);
        assert_eq!(rx.recv().await.unwrap(), vec![0x02]);
        assert_eq!(rx.recv().await.unwrap(), vec![0x03]);
    }

    #[tokio::test]
    async fn flush_stops_at_budget_even_when_channel_has_room() {
        let (tx, mut rx) = frame_channel(100);
        let mut buffer = OutboundFrameBuffer::new(tx, 10, 2);

        buffer.push(vec![0x01]).unwrap();
        buffer.push(vec![0x02]).unwrap();
        buffer.push(vec![0x03]).unwrap();
        buffer.push(vec![0x04]).unwrap();
        buffer.push(vec![0x05]).unwrap();

        buffer.flush();

        assert_eq!(buffer.pending_len(), 3);

        assert_eq!(rx.recv().await.unwrap(), vec![0x01]);
        assert_eq!(rx.recv().await.unwrap(), vec![0x02]);
    }

    #[tokio::test]
    async fn flush_stops_early_when_channel_full() {
        let (tx, _rx) = frame_channel(2);
        let mut buffer = OutboundFrameBuffer::new(tx, 10, 10);

        buffer.push(vec![0x01]).unwrap();
        buffer.push(vec![0x02]).unwrap();
        buffer.push(vec![0x03]).unwrap();
        buffer.push(vec![0x04]).unwrap();
        buffer.push(vec![0x05]).unwrap();

        buffer.flush();

        assert_eq!(buffer.pending_len(), 3);
    }

    #[tokio::test]
    async fn has_room_reflects_pending_queue_state() {
        let (tx, _rx) = frame_channel(16);
        let mut buffer = OutboundFrameBuffer::new(tx, 2, 2);

        assert!(buffer.has_room());

        buffer.push(vec![0x01]).unwrap();
        assert!(buffer.has_room());

        buffer.push(vec![0x02]).unwrap();
        assert!(!buffer.has_room());

        buffer.flush();
        assert!(buffer.has_room());
    }

    #[tokio::test]
    async fn flush_with_empty_pending_is_noop() {
        let (tx, _rx) = frame_channel(16);
        let mut buffer = OutboundFrameBuffer::new(tx, 4, 2);

        buffer.flush();

        assert_eq!(buffer.pending_len(), 0);
    }

    #[tokio::test]
    async fn flush_sends_all_when_fewer_than_budget() {
        let (tx, mut rx) = frame_channel(16);
        let mut buffer = OutboundFrameBuffer::new(tx, 8, 10);

        buffer.push(vec![0x01]).unwrap();
        buffer.push(vec![0x02]).unwrap();
        buffer.push(vec![0x03]).unwrap();

        buffer.flush();

        assert_eq!(buffer.pending_len(), 0);

        assert_eq!(rx.recv().await.unwrap(), vec![0x01]);
        assert_eq!(rx.recv().await.unwrap(), vec![0x02]);
        assert_eq!(rx.recv().await.unwrap(), vec![0x03]);
    }

    #[tokio::test]
    async fn flush_preserves_order_when_channel_full() {
        let (tx, mut rx) = frame_channel(1);
        let mut buffer = OutboundFrameBuffer::new(tx, 10, 10);

        buffer.push(vec![0x01]).unwrap();
        buffer.push(vec![0x02]).unwrap();
        buffer.push(vec![0x03]).unwrap();

        buffer.flush();
        assert_eq!(buffer.pending_len(), 2);
        assert_eq!(rx.recv().await.unwrap(), vec![0x01]);

        buffer.flush();
        assert_eq!(buffer.pending_len(), 1);
        assert_eq!(rx.recv().await.unwrap(), vec![0x02]);

        buffer.flush();
        assert_eq!(buffer.pending_len(), 0);
        assert_eq!(rx.recv().await.unwrap(), vec![0x03]);
    }

    #[tokio::test]
    async fn flush_handles_channel_closed_gracefully() {
        let (tx, rx) = frame_channel(1);
        let mut buffer = OutboundFrameBuffer::new(tx, 10, 10);

        buffer.push(vec![0x01]).unwrap();
        buffer.push(vec![0x02]).unwrap();

        buffer.flush();
        assert_eq!(buffer.pending_len(), 1);

        drop(rx);

        // Next flush should handle closure gracefully (no panic)
        buffer.flush();
    }

    #[tokio::test]
    async fn flush_prioritizes_deferred_over_pending() {
        let (tx, mut rx) = frame_channel(1);
        let mut buffer = OutboundFrameBuffer::new(tx, 10, 10);

        buffer.push(vec![0x01]).unwrap();
        buffer.push(vec![0x02]).unwrap();

        buffer.flush();
        assert_eq!(buffer.pending_len(), 1);

        buffer.push(vec![0x03]).unwrap();
        buffer.push(vec![0x04]).unwrap();

        assert_eq!(rx.recv().await.unwrap(), vec![0x01]);

        // Next flush should send deferred 0x02 first
        buffer.flush();
        assert_eq!(buffer.pending_len(), 2);
        assert_eq!(rx.recv().await.unwrap(), vec![0x02]);

        buffer.flush();
        assert_eq!(buffer.pending_len(), 1);
        assert_eq!(rx.recv().await.unwrap(), vec![0x03]);
    }

    #[tokio::test]
    async fn pop_next_returns_deferred_first() {
        let (tx, _rx) = frame_channel(1);
        let mut buffer = OutboundFrameBuffer::new(tx, 10, 10);

        buffer.push(vec![0x01]).unwrap();
        buffer.push(vec![0x02]).unwrap();
        buffer.push(vec![0x03]).unwrap();

        buffer.flush();
        assert_eq!(buffer.pending_len(), 2);

        assert_eq!(buffer.pop_next(), Some(vec![0x02]));
        assert_eq!(buffer.pop_next(), Some(vec![0x03]));
        assert_eq!(buffer.pop_next(), None);
    }

    #[tokio::test]
    async fn pop_next_returns_pending_when_no_deferred() {
        let (tx, _rx) = frame_channel(16);
        let mut buffer = OutboundFrameBuffer::new(tx, 10, 10);

        buffer.push(vec![0x01]).unwrap();
        buffer.push(vec![0x02]).unwrap();

        // No flush, so no deferred frame
        assert_eq!(buffer.pop_next(), Some(vec![0x01]));
        assert_eq!(buffer.pop_next(), Some(vec![0x02]));
        assert_eq!(buffer.pop_next(), None);
    }
}
