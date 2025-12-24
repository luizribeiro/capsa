use crate::FrameIO;
use smoltcp::phy::{Device, DeviceCapabilities, Medium, RxToken, TxToken};
use smoltcp::time::Instant;
use std::task::{Context, Poll};

/// Wraps a FrameIO to implement smoltcp's Device trait.
pub struct SmoltcpDevice<F: FrameIO> {
    frame_io: F,
    rx_buffer: Vec<u8>,
    rx_len: Option<usize>,
    tx_buffer: Vec<u8>,
}

impl<F: FrameIO> SmoltcpDevice<F> {
    pub fn new(frame_io: F) -> Self {
        let mtu = frame_io.mtu();
        Self {
            frame_io,
            rx_buffer: vec![0u8; mtu + 14], // MTU + ethernet header
            rx_len: None,
            tx_buffer: vec![0u8; mtu + 14],
        }
    }

    /// Poll for incoming frames. Call this before each smoltcp poll.
    pub fn poll_recv(&mut self, cx: &mut Context<'_>) {
        if self.rx_len.is_some() {
            return; // Already have a pending frame
        }

        match self.frame_io.poll_recv(cx, &mut self.rx_buffer) {
            Poll::Ready(Ok(len)) => {
                self.rx_len = Some(len);
            }
            Poll::Ready(Err(e)) => {
                tracing::warn!("Frame receive error: {}", e);
            }
            Poll::Pending => {}
        }
    }

    /// Check if there's a pending frame to process.
    pub fn has_pending_rx(&self) -> bool {
        self.rx_len.is_some()
    }

    /// Peek at the pending received frame without consuming it.
    /// Returns None if no frame is pending.
    pub fn peek_rx(&self) -> Option<&[u8]> {
        self.rx_len.map(|len| &self.rx_buffer[..len])
    }

    /// Discard the pending received frame without processing it.
    /// Use this after handling a frame externally (e.g., for NAT).
    pub fn discard_rx(&mut self) {
        self.rx_len = None;
    }

    /// Send a frame directly, bypassing smoltcp.
    pub fn send_frame(&mut self, frame: &[u8]) -> std::io::Result<()> {
        self.frame_io.send(frame)
    }
}

impl<F: FrameIO> Device for SmoltcpDevice<F> {
    type RxToken<'a>
        = SmoltcpRxToken<'a>
    where
        Self: 'a;
    type TxToken<'a>
        = SmoltcpTxToken<'a, F>
    where
        Self: 'a;

    fn capabilities(&self) -> DeviceCapabilities {
        let mut caps = DeviceCapabilities::default();
        caps.max_transmission_unit = self.frame_io.mtu();
        caps.medium = Medium::Ethernet;
        caps
    }

    fn receive(&mut self, _timestamp: Instant) -> Option<(Self::RxToken<'_>, Self::TxToken<'_>)> {
        let len = self.rx_len.take()?;

        // SAFETY: We're creating two mutable references to different parts of self.
        // rx_buffer is only used by RxToken (read-only), tx_buffer and frame_io by TxToken.
        // This is safe because they don't overlap.
        let rx_token = SmoltcpRxToken {
            buffer: unsafe { std::slice::from_raw_parts(self.rx_buffer.as_ptr(), len) },
        };
        let tx_token = SmoltcpTxToken { device: self };

        Some((rx_token, tx_token))
    }

    fn transmit(&mut self, _timestamp: Instant) -> Option<Self::TxToken<'_>> {
        Some(SmoltcpTxToken { device: self })
    }
}

/// Receive token for smoltcp.
pub struct SmoltcpRxToken<'a> {
    buffer: &'a [u8],
}

impl<'a> RxToken for SmoltcpRxToken<'a> {
    fn consume<R, F>(self, f: F) -> R
    where
        F: FnOnce(&[u8]) -> R,
    {
        f(self.buffer)
    }
}

/// Transmit token for smoltcp.
pub struct SmoltcpTxToken<'a, F: FrameIO> {
    device: &'a mut SmoltcpDevice<F>,
}

impl<F: FrameIO> TxToken for SmoltcpTxToken<'_, F> {
    fn consume<R, Func>(self, len: usize, f: Func) -> R
    where
        Func: FnOnce(&mut [u8]) -> R,
    {
        let buf = &mut self.device.tx_buffer[..len];
        let result = f(buf);
        if let Err(e) = self.device.frame_io.send(buf) {
            tracing::warn!("Frame send error: {}", e);
        }
        result
    }
}
