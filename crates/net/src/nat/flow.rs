use crate::frame::FrameSender;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Instant;
use tokio::net::UdpSocket;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::spawn_nat_forward_task;

pub(super) struct NewFlowParams<F> {
    pub(super) socket: Arc<UdpSocket>,
    pub(super) permit: OwnedSemaphorePermit,
    pub(super) buf_size: usize,
    pub(super) label: &'static str,
    pub(super) handler: F,
}

pub(super) fn acquire_semaphore(
    semaphore: &Arc<Semaphore>,
    label: &str,
    src_label: &dyn std::fmt::Display,
) -> Option<OwnedSemaphorePermit> {
    match semaphore.clone().try_acquire_owned() {
        Ok(p) => Some(p),
        Err(_) => {
            tracing::warn!("NAT: Global task limit reached, rejecting {label} {src_label}");
            None
        }
    }
}

pub(super) fn create_flow<F>(
    params: NewFlowParams<F>,
    tx: FrameSender,
    cancel: CancellationToken,
) -> (Arc<UdpSocket>, JoinHandle<()>, Instant)
where
    F: Fn(&[u8], SocketAddr) -> Option<Vec<u8>> + Send + 'static,
{
    let task_handle = spawn_nat_forward_task(
        params.socket.clone(),
        tx,
        cancel,
        params.permit,
        params.buf_size,
        params.label,
        params.handler,
    );

    (params.socket, task_handle, Instant::now())
}
