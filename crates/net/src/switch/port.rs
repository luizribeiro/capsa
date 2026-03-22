use crate::frame::{EthernetFrameIO, FrameReader, FrameWriter};

use std::io;
use tokio::sync::mpsc;

/// A port on the virtual switch, implementing EthernetFrameIO for VM attachment.
pub struct SwitchPort {
    pub(super) id: usize,
    pub(super) tx: mpsc::Sender<Vec<u8>>,
    pub(super) rx: tokio::sync::Mutex<mpsc::Receiver<Vec<u8>>>,
    pub(super) pending_frame: std::sync::Mutex<Option<Vec<u8>>>,
}

/// Read half of a split `SwitchPort`.
pub struct SwitchPortReader {
    rx: mpsc::Receiver<Vec<u8>>,
    pending_frame: Option<Vec<u8>>,
}

/// Write half of a split `SwitchPort`.
pub struct SwitchPortWriter {
    tx: mpsc::Sender<Vec<u8>>,
}

impl SwitchPort {
    /// Get the port ID.
    pub fn id(&self) -> usize {
        self.id
    }

    /// Get a clone of the sender channel for sending frames to the switch.
    pub fn sender(&self) -> mpsc::Sender<Vec<u8>> {
        self.tx.clone()
    }

    /// Consume this port and return the receiver channel for receiving frames from the switch.
    pub fn into_receiver(self) -> mpsc::Receiver<Vec<u8>> {
        self.rx.into_inner()
    }

    fn take_pending_frame(&self) -> Option<Vec<u8>> {
        self.pending_frame.lock().unwrap().take()
    }
}

impl SwitchPort {
    pub async fn send_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.tx
            .send(frame.to_vec())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "switch closed"))
    }

    pub async fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
        if let Some(frame) = self.take_pending_frame() {
            return Ok(frame);
        }

        self.rx
            .lock()
            .await
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "switch closed"))
    }

    pub fn try_recv_frame(&mut self) -> io::Result<Option<Vec<u8>>> {
        if let Some(frame) = self.take_pending_frame() {
            return Ok(Some(frame));
        }

        let mut rx = match self.rx.try_lock() {
            Ok(rx) => rx,
            Err(_) => return Ok(None),
        };

        match rx.try_recv() {
            Ok(frame) => Ok(Some(frame)),
            Err(mpsc::error::TryRecvError::Empty) => Ok(None),
            Err(mpsc::error::TryRecvError::Disconnected) => {
                Err(io::Error::new(io::ErrorKind::BrokenPipe, "switch closed"))
            }
        }
    }
}

impl EthernetFrameIO for SwitchPort {
    type ReadHalf = SwitchPortReader;
    type WriteHalf = SwitchPortWriter;

    fn mtu(&self) -> usize {
        1500
    }

    fn split(self) -> (Self::ReadHalf, Self::WriteHalf) {
        let pending = self.pending_frame.into_inner().unwrap();
        let rx = self.rx.into_inner();
        (
            SwitchPortReader {
                rx,
                pending_frame: pending,
            },
            SwitchPortWriter { tx: self.tx },
        )
    }
}

impl FrameReader for SwitchPortReader {
    async fn recv_frame(&mut self) -> io::Result<Vec<u8>> {
        if let Some(frame) = self.pending_frame.take() {
            return Ok(frame);
        }

        self.rx
            .recv()
            .await
            .ok_or_else(|| io::Error::new(io::ErrorKind::BrokenPipe, "switch closed"))
    }
}

impl FrameWriter for SwitchPortWriter {
    async fn send_frame(&mut self, frame: &[u8]) -> io::Result<()> {
        self.tx
            .send(frame.to_vec())
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "switch closed"))
    }
}
