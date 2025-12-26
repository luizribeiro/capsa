//! Vsock-to-Unix socket bridging.
//!
//! This module bridges vsock connections to Unix domain sockets,
//! allowing host applications to communicate with guest applications.

use crate::virtio_vsock::{BridgeToDevice, DeviceToBridge};
use capsa_core::VsockPortConfig;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc};

const BRIDGE_BUFFER_SIZE: usize = 4096;

/// Manages vsock port bridging to Unix sockets.
pub struct VsockBridge {
    /// Configured ports and their socket paths
    ports: HashMap<u32, PathBuf>,
}

impl VsockBridge {
    pub fn new(port_configs: Vec<VsockPortConfig>) -> Self {
        let ports = port_configs
            .into_iter()
            .filter(|p| !p.is_connect())
            .map(|p| (p.port(), p.socket_path().to_path_buf()))
            .collect();
        Self { ports }
    }

    /// Run the bridge loop.
    ///
    /// This spawns a Unix socket listener for each configured port and handles
    /// bidirectional data transfer between Unix sockets and the vsock device.
    pub async fn run(
        self,
        device_tx: mpsc::UnboundedSender<BridgeToDevice>,
        mut device_rx: mpsc::UnboundedReceiver<DeviceToBridge>,
    ) {
        // Track active connections: port -> write half of Unix stream
        let connections: Arc<Mutex<HashMap<u32, tokio::io::WriteHalf<UnixStream>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Track pending connections waiting for guest to connect
        let pending_hosts: Arc<Mutex<HashMap<u32, UnixStream>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Spawn listener for each port
        for (port, socket_path) in &self.ports {
            let port = *port;
            let socket_path = socket_path.clone();
            let pending = pending_hosts.clone();
            let connections = connections.clone();
            let device_tx = device_tx.clone();

            tokio::spawn(async move {
                if let Err(e) =
                    run_port_listener(socket_path, port, pending, connections, device_tx).await
                {
                    tracing::error!("[vsock] port {}: listener error: {}", port, e);
                }
            });
        }

        // Main loop: handle messages from device
        while let Some(msg) = device_rx.recv().await {
            match msg {
                DeviceToBridge::Connect { local_port } => {
                    // Guest is connecting - check if host already connected
                    let mut pending = pending_hosts.lock().await;
                    if let Some(stream) = pending.remove(&local_port) {
                        // Host already connected, start bridging
                        let (read_half, write_half) = tokio::io::split(stream);

                        // Store write half for sending data to host
                        connections.lock().await.insert(local_port, write_half);

                        // Spawn task to read from host and send to guest
                        let device_tx = device_tx.clone();
                        let connections_clone = connections.clone();
                        tokio::spawn(async move {
                            read_from_host(read_half, local_port, device_tx, connections_clone)
                                .await;
                        });
                    }
                }
                DeviceToBridge::Data { local_port, data } => {
                    // Data from guest to send to host
                    let mut conns = connections.lock().await;
                    if let Some(write_half) = conns.get_mut(&local_port)
                        && write_half.write_all(&data).await.is_err()
                    {
                        // Host disconnected
                        conns.remove(&local_port);
                        let _ = device_tx.send(BridgeToDevice::Closed { local_port });
                    }
                }
                DeviceToBridge::Shutdown { local_port } => {
                    // Guest closed connection
                    connections.lock().await.remove(&local_port);
                }
            }
        }
    }
}

async fn run_port_listener(
    socket_path: PathBuf,
    port: u32,
    pending: Arc<Mutex<HashMap<u32, UnixStream>>>,
    connections: Arc<Mutex<HashMap<u32, tokio::io::WriteHalf<UnixStream>>>>,
    _device_tx: mpsc::UnboundedSender<BridgeToDevice>,
) -> std::io::Result<()> {
    // Remove existing socket file
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    tracing::debug!("[vsock] port {}: listening on {:?}", port, socket_path);

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tracing::debug!("[vsock] port {}: host connected", port);

                // Check if guest already connected (connection exists)
                let conns = connections.lock().await;
                if conns.contains_key(&port) {
                    // Connection already active, reject new host connection
                    drop(stream);
                    continue;
                }
                drop(conns);

                // Check if another host is pending
                let mut pend = pending.lock().await;
                if pend.contains_key(&port) {
                    // Another host already pending, reject
                    drop(stream);
                    continue;
                }

                // Store as pending until guest connects
                pend.insert(port, stream);
            }
            Err(e) => {
                tracing::error!("[vsock] port {}: accept error: {}", port, e);
            }
        }
    }
}

async fn read_from_host(
    mut read_half: tokio::io::ReadHalf<UnixStream>,
    port: u32,
    device_tx: mpsc::UnboundedSender<BridgeToDevice>,
    connections: Arc<Mutex<HashMap<u32, tokio::io::WriteHalf<UnixStream>>>>,
) {
    let mut buf = [0u8; BRIDGE_BUFFER_SIZE];
    loop {
        match read_half.read(&mut buf).await {
            Ok(0) => {
                // EOF - host disconnected
                connections.lock().await.remove(&port);
                let _ = device_tx.send(BridgeToDevice::Closed { local_port: port });
                break;
            }
            Ok(n) => {
                let _ = device_tx.send(BridgeToDevice::Data {
                    local_port: port,
                    data: buf[..n].to_vec(),
                });
            }
            Err(_) => {
                connections.lock().await.remove(&port);
                let _ = device_tx.send(BridgeToDevice::Closed { local_port: port });
                break;
            }
        }
    }
}
