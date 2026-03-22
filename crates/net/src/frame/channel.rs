//! Frame channel primitives for transporting Ethernet frames between components.
//!
//! This module provides type aliases and a constructor for creating bounded MPSC
//! channels specifically for transporting Ethernet frames (as `Vec<u8>`).
//!
//! ## When to Use
//!
//! Use `frame_channel` for:
//! - Transporting Ethernet frames between network stack components
//! - Frame I/O between host and guest
//! - Switch port channels
//! - NAT response frames
//!
//! Do NOT use for:
//! - TCP connection data channels (different semantics, per-connection lifecycle)
//! - Non-frame data (control messages, metadata, etc.)
//!
//! ## Capacity
//!
//! The capacity parameter is measured in frames, not bytes. Each frame is
//! typically 64-1500 bytes. See `crate::config::channel` (once added) for
//! standard capacities.

use tokio::sync::mpsc;

/// Sender half of a frame channel for transporting Ethernet frames.
pub type FrameSender = mpsc::Sender<Vec<u8>>;

/// Receiver half of a frame channel for transporting Ethernet frames.
pub type FrameReceiver = mpsc::Receiver<Vec<u8>>;

/// Creates a bounded channel for transporting Ethernet frames.
///
/// The capacity is measured in frames, not bytes.
pub fn frame_channel(capacity: usize) -> (FrameSender, FrameReceiver) {
    mpsc::channel(capacity)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_channel_creation() {
        let (tx, _rx) = frame_channel(16);
        assert!(!tx.is_closed());
    }

    #[tokio::test]
    async fn test_send_and_receive() {
        let (tx, mut rx) = frame_channel(16);

        let frame = vec![0x01, 0x02, 0x03, 0x04];
        tx.send(frame.clone()).await.unwrap();

        let received = rx.recv().await.unwrap();
        assert_eq!(received, frame);
    }

    #[tokio::test]
    async fn test_multiple_frames() {
        let (tx, mut rx) = frame_channel(16);

        let frames: Vec<Vec<u8>> = vec![vec![0x01, 0x02], vec![0x03, 0x04, 0x05], vec![0x06]];

        for frame in &frames {
            tx.send(frame.clone()).await.unwrap();
        }

        for expected in &frames {
            let received = rx.recv().await.unwrap();
            assert_eq!(&received, expected);
        }
    }

    #[tokio::test]
    async fn test_channel_closes_when_sender_dropped() {
        let (tx, mut rx) = frame_channel(16);
        drop(tx);
        assert!(rx.recv().await.is_none());
    }

    #[tokio::test]
    async fn test_try_send_returns_error_when_full() {
        let (tx, _rx) = frame_channel(1);

        tx.send(vec![0x01]).await.unwrap();

        let result = tx.try_send(vec![0x02]);
        assert!(matches!(result, Err(mpsc::error::TrySendError::Full(_))));
    }

    #[tokio::test]
    async fn test_send_returns_error_when_receiver_dropped() {
        let (tx, rx) = frame_channel(16);
        drop(rx);

        let result = tx.send(vec![0x01]).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_try_send_returns_closed_when_receiver_dropped() {
        let (tx, rx) = frame_channel(16);
        drop(rx);

        let result = tx.try_send(vec![0x01]);
        assert!(matches!(result, Err(mpsc::error::TrySendError::Closed(_))));
    }
}
