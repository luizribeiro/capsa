use std::future::Future;
use std::io;

use smoltcp::time::Instant;
use tokio::sync::mpsc;

/// Abstraction for ethernet frame transport.
///
/// This trait allows the network stack to work with different frame
/// sources: socketpairs on macOS, TAP devices on Linux, or virtual
/// switch ports for multi-VM networking.
pub trait EthernetFrameIO: Send + 'static {
    /// Read half type returned by `split()`.
    type ReadHalf: FrameReader;
    /// Write half type returned by `split()`.
    type WriteHalf: FrameWriter;

    /// Maximum transmission unit (typically 1500 for ethernet).
    fn mtu(&self) -> usize {
        1500
    }

    /// Split into independent read and write halves for concurrent I/O.
    ///
    /// This enables running receive and transmit in separate tasks to avoid
    /// deadlocks when both directions are blocked on backpressure.
    fn split(self) -> (Self::ReadHalf, Self::WriteHalf);
}

/// Read half of a split `EthernetFrameIO`.
pub trait FrameReader: Send + 'static {
    /// Async receive - waits for a frame to be available.
    fn recv_frame(&mut self) -> impl Future<Output = io::Result<Vec<u8>>> + Send;
}

/// Write half of a split `EthernetFrameIO`.
pub trait FrameWriter: Send + 'static {
    /// Async send - waits for the transport to be writable if needed.
    fn send_frame(&mut self, frame: &[u8]) -> impl Future<Output = io::Result<()>> + Send;
}

/// Background task that receives frames from guest and sends to the stack.
///
/// This is the RX half of the frame I/O, running independently from TX to
/// avoid deadlocks when both directions experience backpressure.
pub async fn frame_rx_task<R: FrameReader>(mut reader: R, tx_from_guest: mpsc::Sender<Vec<u8>>) {
    loop {
        match reader.recv_frame().await {
            Ok(frame) => {
                // Bounded send with backpressure - waits if channel is full.
                // This propagates backpressure to the guest via socketpair.
                if tx_from_guest.send(frame).await.is_err() {
                    tracing::debug!("Stack channel closed, stopping RX task");
                    break;
                }
            }
            Err(e) => {
                tracing::warn!("Frame receive error: {}, stopping RX task", e);
                break;
            }
        }
    }
}

/// Background task that receives frames from the stack and sends to guest.
///
/// This is the TX half of the frame I/O, running independently from RX to
/// avoid deadlocks when both directions experience backpressure.
pub async fn frame_tx_task<W: FrameWriter>(
    mut writer: W,
    mut rx_to_guest: mpsc::Receiver<Vec<u8>>,
) {
    loop {
        match rx_to_guest.recv().await {
            Some(frame) => {
                // Use send_frame() to wait for the socket to drain when the buffer is full.
                if let Err(e) = writer.send_frame(&frame).await {
                    tracing::warn!("Frame send error: {}, stopping TX task", e);
                    break;
                }
            }
            None => {
                tracing::debug!("Transmit channel closed, stopping TX task");
                break;
            }
        }
    }
}

/// Spawn split frame I/O tasks for independent RX and TX processing.
///
/// Returns handles to both tasks. Splitting RX and TX into separate tasks
/// prevents deadlocks that occur when both directions are blocked on
/// backpressure simultaneously.
pub fn spawn_frame_io_tasks<F: EthernetFrameIO>(
    frame_io: F,
    tx_from_guest: mpsc::Sender<Vec<u8>>,
    rx_to_guest: mpsc::Receiver<Vec<u8>>,
) -> (tokio::task::JoinHandle<()>, tokio::task::JoinHandle<()>) {
    let (reader, writer) = frame_io.split();

    let rx_handle = tokio::spawn(frame_rx_task(reader, tx_from_guest));
    let tx_handle = tokio::spawn(frame_tx_task(writer, rx_to_guest));

    (rx_handle, tx_handle)
}

/// Convert system time to smoltcp Instant.
pub fn smoltcp_now(start: std::time::Instant) -> Instant {
    let elapsed = start.elapsed();
    Instant::from_millis(elapsed.as_millis() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    struct MockReader {
        frames: VecDeque<io::Result<Vec<u8>>>,
    }

    impl FrameReader for MockReader {
        async fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
            self.frames
                .pop_front()
                .unwrap_or_else(|| Err(io::Error::new(io::ErrorKind::BrokenPipe, "eof")))
        }
    }

    struct MockWriter {
        writes: Arc<Mutex<Vec<Vec<u8>>>>,
        fail_after: usize,
    }

    impl FrameWriter for MockWriter {
        async fn send_frame(&mut self, frame: &[u8]) -> io::Result<()> {
            let mut writes = self.writes.lock().unwrap();
            if writes.len() >= self.fail_after {
                return Err(io::Error::other("injected failure"));
            }
            writes.push(frame.to_vec());
            Ok(())
        }
    }

    #[tokio::test]
    async fn frame_rx_task_forwards_frames_until_reader_errors() {
        let reader = MockReader {
            frames: VecDeque::from(vec![
                Ok(vec![1, 2, 3]),
                Err(io::Error::new(io::ErrorKind::UnexpectedEof, "done")),
            ]),
        };
        let (tx, mut rx) = mpsc::channel(4);

        frame_rx_task(reader, tx).await;

        let received = rx.recv().await.expect("expected forwarded frame");
        assert_eq!(received, vec![1, 2, 3]);
        assert!(rx.try_recv().is_err(), "only one frame should be forwarded");
    }

    #[tokio::test]
    async fn frame_tx_task_stops_after_writer_error() {
        let writes = Arc::new(Mutex::new(Vec::new()));
        let writer = MockWriter {
            writes: Arc::clone(&writes),
            fail_after: 1,
        };
        let (tx, rx) = mpsc::channel(4);
        tx.send(vec![1]).await.unwrap();
        tx.send(vec![2]).await.unwrap();

        frame_tx_task(writer, rx).await;

        let writes = writes.lock().unwrap();
        assert_eq!(writes.len(), 1);
        assert_eq!(writes[0], vec![1]);
    }
}
