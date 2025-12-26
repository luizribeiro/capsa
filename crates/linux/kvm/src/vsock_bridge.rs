//! Vsock-to-Unix socket bridging.
//!
//! This module bridges vsock connections to Unix domain sockets,
//! allowing host applications to communicate with guest applications.

use crate::virtio::{BridgeToDevice, DeviceToBridge};
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

        // Track pending guest connections waiting for host to connect
        let pending_guests: Arc<Mutex<std::collections::HashSet<u32>>> =
            Arc::new(Mutex::new(std::collections::HashSet::new()));

        // Spawn listener for each port
        for (port, socket_path) in &self.ports {
            let port = *port;
            let socket_path = socket_path.clone();
            let pending_hosts_clone = pending_hosts.clone();
            let pending_guests_clone = pending_guests.clone();
            let connections = connections.clone();
            let device_tx = device_tx.clone();

            tokio::spawn(async move {
                if let Err(e) = run_port_listener(
                    socket_path,
                    port,
                    pending_hosts_clone,
                    pending_guests_clone,
                    connections,
                    device_tx,
                )
                .await
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
                    } else {
                        // Host hasn't connected yet - mark guest as waiting
                        pending_guests.lock().await.insert(local_port);
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
    pending_hosts: Arc<Mutex<HashMap<u32, UnixStream>>>,
    pending_guests: Arc<Mutex<std::collections::HashSet<u32>>>,
    connections: Arc<Mutex<HashMap<u32, tokio::io::WriteHalf<UnixStream>>>>,
    device_tx: mpsc::UnboundedSender<BridgeToDevice>,
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
                let mut pend = pending_hosts.lock().await;
                if pend.contains_key(&port) {
                    // Another host already pending, reject
                    drop(stream);
                    continue;
                }

                // Check if guest is already waiting for host
                let mut guests = pending_guests.lock().await;
                if guests.remove(&port) {
                    // Guest already connected, start bridging immediately
                    drop(pend);
                    drop(guests);

                    let (read_half, write_half) = tokio::io::split(stream);
                    connections.lock().await.insert(port, write_half);

                    let device_tx = device_tx.clone();
                    let connections_clone = connections.clone();
                    tokio::spawn(async move {
                        read_from_host(read_half, port, device_tx, connections_clone).await;
                    });
                } else {
                    // Store as pending until guest connects
                    drop(guests);
                    pend.insert(port, stream);
                }
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;

    #[tokio::test]
    async fn guest_connects_before_host() {
        let socket_path =
            std::env::temp_dir().join(format!("vsock_test_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);

        let port_config = VsockPortConfig::listen(1024, socket_path.clone());
        let bridge = VsockBridge::new(vec![port_config]);

        let (device_tx, mut device_rx) = mpsc::unbounded_channel::<BridgeToDevice>();
        let (bridge_tx, bridge_rx) = mpsc::unbounded_channel::<DeviceToBridge>();

        // Start the bridge
        tokio::spawn(async move {
            bridge.run(device_tx, bridge_rx).await;
        });

        // Wait for the Unix socket listener to be ready
        tokio::time::sleep(Duration::from_millis(50)).await;

        // GUEST CONNECTS FIRST (before host)
        bridge_tx
            .send(DeviceToBridge::Connect { local_port: 1024 })
            .unwrap();

        // Give the bridge time to process the Connect message
        tokio::time::sleep(Duration::from_millis(50)).await;

        // NOW host connects (after guest)
        let mut host_stream = UnixStream::connect(&socket_path)
            .await
            .expect("Failed to connect to Unix socket");

        // Give the bridge time to set up the connection
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Host sends data
        host_stream
            .write_all(b"hello from host")
            .await
            .expect("Failed to write");

        // Bridge should forward data to device
        let msg = timeout(Duration::from_secs(1), device_rx.recv())
            .await
            .expect("Timeout waiting for data")
            .expect("Channel closed");

        match msg {
            BridgeToDevice::Data { local_port, data } => {
                assert_eq!(local_port, 1024);
                assert_eq!(data, b"hello from host");
            }
            _ => panic!("Expected Data message, got {:?}", msg),
        }

        // Clean up
        let _ = std::fs::remove_file(&socket_path);
    }

    #[tokio::test]
    async fn host_connects_before_guest() {
        let socket_path =
            std::env::temp_dir().join(format!("vsock_test2_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);

        let port_config = VsockPortConfig::listen(1025, socket_path.clone());
        let bridge = VsockBridge::new(vec![port_config]);

        let (device_tx, mut device_rx) = mpsc::unbounded_channel::<BridgeToDevice>();
        let (bridge_tx, bridge_rx) = mpsc::unbounded_channel::<DeviceToBridge>();

        // Start the bridge
        tokio::spawn(async move {
            bridge.run(device_tx, bridge_rx).await;
        });

        // Wait for the Unix socket listener to be ready
        tokio::time::sleep(Duration::from_millis(50)).await;

        // HOST CONNECTS FIRST
        let mut host_stream = UnixStream::connect(&socket_path)
            .await
            .expect("Failed to connect to Unix socket");

        // Give the bridge time to accept the connection
        tokio::time::sleep(Duration::from_millis(50)).await;

        // NOW guest connects (after host)
        bridge_tx
            .send(DeviceToBridge::Connect { local_port: 1025 })
            .unwrap();

        // Give the bridge time to set up the connection
        tokio::time::sleep(Duration::from_millis(50)).await;

        // Host sends data
        host_stream
            .write_all(b"hello from host")
            .await
            .expect("Failed to write");

        // Bridge should forward data to device
        let msg = timeout(Duration::from_secs(1), device_rx.recv())
            .await
            .expect("Timeout waiting for data")
            .expect("Channel closed");

        match msg {
            BridgeToDevice::Data { local_port, data } => {
                assert_eq!(local_port, 1025);
                assert_eq!(data, b"hello from host");
            }
            _ => panic!("Expected Data message, got {:?}", msg),
        }

        // Clean up
        let _ = std::fs::remove_file(&socket_path);
    }
}
