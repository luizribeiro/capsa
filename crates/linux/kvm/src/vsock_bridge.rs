//! Vsock-to-Unix socket bridging.
//!
//! This module bridges vsock connections to Unix domain sockets,
//! allowing host applications to communicate with guest applications.
//!
//! Two modes are supported:
//! - **Listen mode**: Guest connects to host. Host accepts on Unix socket.
//! - **Connect mode**: Host connects to guest. Host initiates via Unix socket.

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
    /// Listen-mode ports (guest connects to host)
    listen_ports: HashMap<u32, PathBuf>,
    /// Connect-mode ports (host connects to guest)
    connect_ports: HashMap<u32, PathBuf>,
}

impl VsockBridge {
    pub fn new(port_configs: Vec<VsockPortConfig>) -> Self {
        let mut listen_ports = HashMap::new();
        let mut connect_ports = HashMap::new();

        for p in port_configs {
            if p.is_connect() {
                connect_ports.insert(p.port(), p.socket_path().to_path_buf());
            } else {
                listen_ports.insert(p.port(), p.socket_path().to_path_buf());
            }
        }

        Self {
            listen_ports,
            connect_ports,
        }
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

        // Track pending connections waiting for guest to connect (listen mode)
        let pending_hosts: Arc<Mutex<HashMap<u32, UnixStream>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Track pending guest connections waiting for host to connect (listen mode)
        let pending_guests: Arc<Mutex<std::collections::HashSet<u32>>> =
            Arc::new(Mutex::new(std::collections::HashSet::new()));

        // Track pending connect-mode connections waiting for guest to accept
        let pending_connect_hosts: Arc<Mutex<HashMap<u32, UnixStream>>> =
            Arc::new(Mutex::new(HashMap::new()));

        // Spawn listener for each listen-mode port
        for (port, socket_path) in &self.listen_ports {
            let port = *port;
            let socket_path = socket_path.clone();
            let pending_hosts_clone = pending_hosts.clone();
            let pending_guests_clone = pending_guests.clone();
            let connections = connections.clone();
            let device_tx = device_tx.clone();

            tokio::spawn(async move {
                if let Err(e) = run_listen_port(
                    socket_path,
                    port,
                    pending_hosts_clone,
                    pending_guests_clone,
                    connections,
                    device_tx,
                )
                .await
                {
                    tracing::error!("[vsock] listen port {}: error: {}", port, e);
                }
            });
        }

        // Spawn listener for each connect-mode port
        for (port, socket_path) in &self.connect_ports {
            let port = *port;
            let socket_path = socket_path.clone();
            let pending_connect_hosts_clone = pending_connect_hosts.clone();
            let connections = connections.clone();
            let device_tx = device_tx.clone();

            tokio::spawn(async move {
                if let Err(e) = run_connect_port(
                    socket_path,
                    port,
                    pending_connect_hosts_clone,
                    connections,
                    device_tx,
                )
                .await
                {
                    tracing::error!("[vsock] connect port {}: error: {}", port, e);
                }
            });
        }

        // Main loop: handle messages from device
        while let Some(msg) = device_rx.recv().await {
            match msg {
                DeviceToBridge::Connect { local_port } => {
                    // Guest is connecting (listen mode) - check if host already connected
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
                DeviceToBridge::Connected {
                    local_port,
                    _peer_port: _,
                } => {
                    // Guest accepted our connection (connect mode)
                    let mut pending = pending_connect_hosts.lock().await;
                    if let Some(stream) = pending.remove(&local_port) {
                        let (read_half, write_half) = tokio::io::split(stream);

                        connections.lock().await.insert(local_port, write_half);

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
                    pending_connect_hosts.lock().await.remove(&local_port);
                }
            }
        }
    }
}

/// Listen-mode port handler.
/// Waits for host to connect, then coordinates with device for guest connection.
async fn run_listen_port(
    socket_path: PathBuf,
    port: u32,
    pending_hosts: Arc<Mutex<HashMap<u32, UnixStream>>>,
    pending_guests: Arc<Mutex<std::collections::HashSet<u32>>>,
    connections: Arc<Mutex<HashMap<u32, tokio::io::WriteHalf<UnixStream>>>>,
    device_tx: mpsc::UnboundedSender<BridgeToDevice>,
) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    tracing::debug!(
        "[vsock] listen port {}: listening on {:?}",
        port,
        socket_path
    );

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tracing::debug!("[vsock] listen port {}: host connected", port);

                let conns = connections.lock().await;
                if conns.contains_key(&port) {
                    drop(stream);
                    continue;
                }
                drop(conns);

                let mut pend = pending_hosts.lock().await;
                if pend.contains_key(&port) {
                    drop(stream);
                    continue;
                }

                let mut guests = pending_guests.lock().await;
                if guests.remove(&port) {
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
                    drop(guests);
                    pend.insert(port, stream);
                }
            }
            Err(e) => {
                tracing::error!("[vsock] listen port {}: accept error: {}", port, e);
            }
        }
    }
}

/// Connect-mode port handler.
/// When host connects, sends connection request to device (which forwards to guest).
async fn run_connect_port(
    socket_path: PathBuf,
    port: u32,
    pending_connect_hosts: Arc<Mutex<HashMap<u32, UnixStream>>>,
    connections: Arc<Mutex<HashMap<u32, tokio::io::WriteHalf<UnixStream>>>>,
    device_tx: mpsc::UnboundedSender<BridgeToDevice>,
) -> std::io::Result<()> {
    let _ = std::fs::remove_file(&socket_path);

    let listener = UnixListener::bind(&socket_path)?;
    tracing::debug!(
        "[vsock] connect port {}: listening on {:?}",
        port,
        socket_path
    );

    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                tracing::debug!("[vsock] connect port {}: host connected", port);

                // Check if connection already active
                let conns = connections.lock().await;
                if conns.contains_key(&port) {
                    drop(stream);
                    continue;
                }
                drop(conns);

                // Check if another host is already pending
                let mut pending = pending_connect_hosts.lock().await;
                if pending.contains_key(&port) {
                    drop(stream);
                    continue;
                }

                // Store stream as pending and request connection to guest
                pending.insert(port, stream);
                drop(pending);

                // Tell device to initiate connection to guest
                let _ = device_tx.send(BridgeToDevice::Connect { local_port: port });
            }
            Err(e) => {
                tracing::error!("[vsock] connect port {}: accept error: {}", port, e);
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

    #[tokio::test]
    async fn connect_mode_host_to_guest() {
        let socket_path =
            std::env::temp_dir().join(format!("vsock_test3_{}.sock", std::process::id()));
        let _ = std::fs::remove_file(&socket_path);

        // Use connect mode (host initiates connection to guest)
        let port_config = VsockPortConfig::connect(2048, socket_path.clone());
        let bridge = VsockBridge::new(vec![port_config]);

        let (device_tx, mut device_rx) = mpsc::unbounded_channel::<BridgeToDevice>();
        let (bridge_tx, bridge_rx) = mpsc::unbounded_channel::<DeviceToBridge>();

        tokio::spawn(async move {
            bridge.run(device_tx, bridge_rx).await;
        });

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Host connects to Unix socket
        let mut host_stream = UnixStream::connect(&socket_path)
            .await
            .expect("Failed to connect to Unix socket");

        // Bridge should send Connect to device (requesting connection to guest)
        let msg = timeout(Duration::from_secs(1), device_rx.recv())
            .await
            .expect("Timeout waiting for Connect")
            .expect("Channel closed");

        match msg {
            BridgeToDevice::Connect { local_port } => {
                assert_eq!(local_port, 2048);
            }
            _ => panic!("Expected Connect message, got {:?}", msg),
        }

        // Simulate guest accepting connection
        bridge_tx
            .send(DeviceToBridge::Connected {
                local_port: 2048,
                _peer_port: 49152,
            })
            .unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        // Now host can send data
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
                assert_eq!(local_port, 2048);
                assert_eq!(data, b"hello from host");
            }
            _ => panic!("Expected Data message, got {:?}", msg),
        }

        let _ = std::fs::remove_file(&socket_path);
    }
}
