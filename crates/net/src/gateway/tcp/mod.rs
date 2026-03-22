mod connection;
mod host_io;

pub use connection::InitiateResult;
use connection::{
    ConnectResult, ConnectionState, FlowKey, CONNECTED_HANDSHAKE_TIMEOUT, MAX_PENDING_CONNECTS,
    MAX_TCP_CONNECTIONS, SMOLTCP_SOCKET_BUF, TCP_CONNECT_TIMEOUT, UNSENT_PAUSE_THRESHOLD,
    UNSENT_RESUME_THRESHOLD,
};
pub use host_io::TcpHostEvent;

use smoltcp::iface::{SocketHandle, SocketSet};
use smoltcp::socket::tcp;
use std::collections::HashMap;
use std::net::{SocketAddr, SocketAddrV4};
use std::time::{Duration, Instant};
use tokio::net::TcpStream;
use tokio::sync::mpsc;

pub struct PortForwardRequest {
    pub stream: TcpStream,
    pub guest_ip: std::net::Ipv4Addr,
    pub guest_port: u16,
}

pub(crate) fn new_smoltcp_tcp_socket() -> tcp::Socket<'static> {
    let rx_buf = tcp::SocketBuffer::new(vec![0u8; SMOLTCP_SOCKET_BUF]);
    let tx_buf = tcp::SocketBuffer::new(vec![0u8; SMOLTCP_SOCKET_BUF]);
    let mut socket = tcp::Socket::new(rx_buf, tx_buf);
    socket.set_nagle_enabled(false);
    socket.set_ack_delay(None);
    socket
}

pub struct TcpManager {
    connections: HashMap<SocketHandle, ConnectionState>,
    flow_index: HashMap<FlowKey, SocketHandle>,
    connect_result_rx: mpsc::Receiver<ConnectResult>,
    connect_result_tx: mpsc::Sender<ConnectResult>,
    next_ephemeral_port: u16,
}

impl TcpManager {
    pub fn new() -> Self {
        let (tx, rx) = mpsc::channel(MAX_PENDING_CONNECTS);
        Self {
            connections: HashMap::new(),
            flow_index: HashMap::new(),
            connect_result_rx: rx,
            connect_result_tx: tx,
            next_ephemeral_port: 0,
        }
    }

    pub fn initiate(
        &mut self,
        socket: tcp::Socket<'static>,
        remote_addr: SocketAddrV4,
        guest_src: SocketAddrV4,
        sockets: &mut SocketSet<'_>,
    ) -> InitiateResult {
        let flow_key = FlowKey {
            guest_src,
            remote_dst: remote_addr,
        };

        if self.flow_index.contains_key(&flow_key) {
            tracing::debug!(
                src = %guest_src,
                dst = %remote_addr,
                "TCP: SYN dedup hit, flow exists"
            );
            return InitiateResult::DuplicateFlow;
        }

        if self.connections.len() >= MAX_TCP_CONNECTIONS {
            tracing::warn!(
                "TCP manager: connection limit reached ({}), rejecting",
                MAX_TCP_CONNECTIONS
            );
            return InitiateResult::RejectedLimit;
        }

        let handle = sockets.add(socket);
        let tx = self.connect_result_tx.clone();
        let task = crate::util::spawn_named("tcp-manager-connect", async move {
            let result = tokio::time::timeout(
                TCP_CONNECT_TIMEOUT,
                TcpStream::connect(SocketAddr::V4(remote_addr)),
            )
            .await;

            let stream_result = match result {
                Ok(Ok(stream)) => Ok(stream),
                Ok(Err(e)) => {
                    tracing::debug!("TCP manager: connect to {} failed: {}", remote_addr, e);
                    Err(())
                }
                Err(_) => {
                    tracing::debug!("TCP manager: connect to {} timed out", remote_addr);
                    Err(())
                }
            };

            let _ = tx
                .send(ConnectResult {
                    handle,
                    result: stream_result,
                })
                .await;
        });

        self.connections.insert(
            handle,
            ConnectionState::Pending {
                task,
                created: Instant::now(),
            },
        );

        self.flow_index.insert(flow_key, handle);

        tracing::debug!(
            active = self.connections.len(),
            "TCP: new outbound connection"
        );

        InitiateResult::Created(handle)
    }

    pub fn register_host_stream(
        &mut self,
        socket: tcp::Socket<'static>,
        stream: TcpStream,
        sockets: &mut SocketSet<'_>,
    ) -> Option<SocketHandle> {
        if self.connections.len() >= MAX_TCP_CONNECTIONS {
            tracing::warn!(
                "TCP manager: connection limit reached ({}), rejecting port forward",
                MAX_TCP_CONNECTIONS
            );
            return None;
        }
        let handle = sockets.add(socket);
        self.connections.insert(
            handle,
            ConnectionState::Connected {
                stream,
                created: Instant::now(),
            },
        );
        Some(handle)
    }

    pub fn poll_connect_results(&mut self, sockets: &mut SocketSet<'_>) {
        while let Ok(cr) = self.connect_result_rx.try_recv() {
            let Some(state) = self.connections.get(&cr.handle) else {
                continue;
            };
            if !matches!(state, ConnectionState::Pending { .. }) {
                continue;
            }

            match cr.result {
                Ok(stream) => {
                    self.connections.insert(
                        cr.handle,
                        ConnectionState::Connected {
                            stream,
                            created: Instant::now(),
                        },
                    );
                }
                Err(()) => {
                    tracing::debug!(handle = ?cr.handle, "TCP: host connect failed, removing");
                    self.connections.remove(&cr.handle);
                    self.flow_index.retain(|_, h| *h != cr.handle);
                    let socket = sockets.get_mut::<tcp::Socket>(cr.handle);
                    socket.abort();
                    sockets.remove(cr.handle);
                }
            }
        }
    }

    pub fn poll_newly_established(
        &mut self,
        sockets: &mut SocketSet<'_>,
        host_event_tx: &mpsc::Sender<TcpHostEvent>,
    ) {
        let handles: Vec<SocketHandle> = self
            .connections
            .iter()
            .filter_map(|(h, state)| {
                if matches!(state, ConnectionState::Connected { .. }) {
                    Some(*h)
                } else {
                    None
                }
            })
            .collect();

        for handle in handles {
            let socket = sockets.get_mut::<tcp::Socket>(handle);
            if socket.state() != tcp::State::Established {
                continue;
            }

            let ConnectionState::Connected { stream, .. } =
                self.connections.remove(&handle).unwrap()
            else {
                unreachable!();
            };

            let (read_half, write_half) = stream.into_split();
            let (write_tx, write_rx) = mpsc::channel::<Vec<u8>>(64);
            let (pause_tx, pause_rx) = tokio::sync::watch::channel(false);
            let event_tx = host_event_tx.clone();

            let read_handle = handle;
            let host_read = crate::util::spawn_named("tcp-manager-host-read", async move {
                host_io::host_read_task(read_half, event_tx, read_handle, pause_rx).await;
            });

            let host_write = crate::util::spawn_named("tcp-manager-host-write", async move {
                host_io::host_write_task(write_half, write_rx).await;
            });

            self.connections.insert(
                handle,
                ConnectionState::Active {
                    host_read,
                    host_write,
                    host_write_tx: Some(write_tx),
                    unsent: std::collections::VecDeque::new(),
                    read_paused: pause_tx,
                },
            );
        }
    }

    pub fn poll_sockets(&mut self, sockets: &mut SocketSet<'_>) {
        let handles: Vec<SocketHandle> = self.connections.keys().copied().collect();

        for handle in handles {
            let Some(ConnectionState::Active {
                host_write_tx,
                unsent,
                read_paused,
                ..
            }) = self.connections.get_mut(&handle)
            else {
                continue;
            };

            let socket = sockets.get_mut::<tcp::Socket>(handle);

            host_io::drain_unsent(socket, unsent);

            if *read_paused.borrow() && unsent.len() <= UNSENT_RESUME_THRESHOLD {
                let _ = read_paused.send(false);
                tracing::debug!(
                    handle = ?handle,
                    unsent = unsent.len(),
                    "TCP: resuming host reads (unsent buffer drained)"
                );
            }

            if let Some(tx) = host_write_tx {
                while socket.can_recv() {
                    match socket.recv(|buf| {
                        let data = buf.to_vec();
                        (buf.len(), data)
                    }) {
                        Ok(data) if !data.is_empty() => {
                            if tx.try_send(data).is_err() {
                                break;
                            }
                        }
                        _ => break,
                    }
                }
            }

            if !socket.may_recv() && socket.state() != tcp::State::Listen {
                if let Some(ConnectionState::Active { host_write_tx, .. }) =
                    self.connections.get_mut(&handle)
                {
                    host_write_tx.take();
                }
            }
        }
    }

    pub fn handle_host_event(&mut self, event: TcpHostEvent, sockets: &mut SocketSet<'_>) {
        match event {
            TcpHostEvent::Data { handle, data } => {
                if let Some(ConnectionState::Active {
                    unsent,
                    read_paused,
                    ..
                }) = self.connections.get_mut(&handle)
                {
                    unsent.extend(&data);
                    let socket = sockets.get_mut::<tcp::Socket>(handle);
                    host_io::drain_unsent(socket, unsent);

                    if !*read_paused.borrow() && unsent.len() > UNSENT_PAUSE_THRESHOLD {
                        let _ = read_paused.send(true);
                        tracing::debug!(
                            handle = ?handle,
                            unsent = unsent.len(),
                            "TCP: pausing host reads (unsent buffer above threshold)"
                        );
                    }
                }
            }
            TcpHostEvent::Eof { handle } => {
                if let Some(ConnectionState::Active { .. }) = self.connections.get(&handle) {
                    let socket = sockets.get_mut::<tcp::Socket>(handle);
                    socket.close();
                }
            }
        }
    }

    pub fn allocate_local_port(&mut self, sockets: &SocketSet<'_>) -> Option<u16> {
        let connections = &self.connections;
        crate::config::allocate_ephemeral_port(&mut self.next_ephemeral_port, |candidate| {
            connections.keys().any(|handle| {
                let socket = sockets.get::<tcp::Socket>(*handle);
                socket
                    .local_endpoint()
                    .is_some_and(|ep| ep.port == candidate)
            })
        })
    }

    pub fn cleanup(&mut self, sockets: &mut SocketSet<'_>) {
        let stale: Vec<SocketHandle> = self
            .connections
            .iter()
            .filter_map(|(handle, state)| {
                match state {
                    ConnectionState::Pending { created, .. } => {
                        if created.elapsed() > TCP_CONNECT_TIMEOUT + Duration::from_secs(5) {
                            tracing::debug!(handle = ?handle, "TCP cleanup: pending connect timed out");
                            return Some(*handle);
                        }
                    }
                    ConnectionState::Active {
                        host_read,
                        host_write,
                        ..
                    } => {
                        let socket = sockets.get_mut::<tcp::Socket>(*handle);
                        let closed =
                            matches!(socket.state(), tcp::State::Closed | tcp::State::TimeWait);
                        if closed && host_read.is_finished() && host_write.is_finished() {
                            tracing::debug!(handle = ?handle, "TCP cleanup: connection closed and reaped");
                            return Some(*handle);
                        }
                    }
                    ConnectionState::Connected { created, .. } => {
                        if created.elapsed() > CONNECTED_HANDSHAKE_TIMEOUT {
                            tracing::debug!(handle = ?handle, "TCP cleanup: smoltcp handshake timed out");
                            return Some(*handle);
                        }
                    }
                }
                None
            })
            .collect();

        for handle in stale {
            if let Some(state) = self.connections.remove(&handle) {
                self.flow_index.retain(|_, h| *h != handle);
                connection::abort_connection(state);
                sockets.remove(handle);
            }
        }
    }
}

impl Drop for TcpManager {
    fn drop(&mut self) {
        self.flow_index.clear();
        for (_, state) in self.connections.drain() {
            connection::abort_connection(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::iface::SocketSet;
    use std::collections::VecDeque;
    use std::net::SocketAddr;
    use std::time::{Duration, Instant};
    use tokio::net::TcpStream;

    fn dummy_guest_src(port: u16) -> SocketAddrV4 {
        SocketAddrV4::new(std::net::Ipv4Addr::new(10, 0, 2, 15), port)
    }

    #[test]
    fn new_socket_is_tuned_for_throughput() {
        let socket = new_smoltcp_tcp_socket();
        assert!(!socket.nagle_enabled());
        assert_eq!(socket.ack_delay(), None);
    }

    #[tokio::test]
    async fn initiate_adds_pending_connection() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();

        let addr = SocketAddrV4::new(std::net::Ipv4Addr::new(127, 0, 0, 1), 1);
        let result = manager.initiate(socket, addr, dummy_guest_src(1000), &mut sockets);

        let handle = match result {
            InitiateResult::Created(h) => h,
            _ => panic!("expected Created"),
        };

        assert_eq!(manager.connections.len(), 1);
        assert!(matches!(
            manager.connections.get(&handle),
            Some(ConnectionState::Pending { .. })
        ));
    }

    #[tokio::test]
    async fn connect_success_transitions_to_connected() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let addr_v4 = match addr {
            SocketAddr::V4(a) => a,
            _ => panic!("expected v4"),
        };

        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();

        let result = manager.initiate(socket, addr_v4, dummy_guest_src(2000), &mut sockets);
        let handle = match result {
            InitiateResult::Created(h) => h,
            _ => panic!("expected Created"),
        };

        let (_accepted, _peer) = listener.accept().await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        manager.poll_connect_results(&mut sockets);
        assert!(matches!(
            manager.connections.get(&handle),
            Some(ConnectionState::Connected { .. })
        ));
    }

    #[tokio::test]
    async fn connect_failure_aborts_smoltcp_socket() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();

        let addr = SocketAddrV4::new(std::net::Ipv4Addr::new(127, 0, 0, 1), 1);
        let result = manager.initiate(socket, addr, dummy_guest_src(3000), &mut sockets);
        assert!(matches!(result, InitiateResult::Created(_)));

        tokio::time::sleep(Duration::from_millis(200)).await;

        manager.poll_connect_results(&mut sockets);

        assert!(manager.connections.is_empty());
    }

    fn insert_active(
        manager: &mut TcpManager,
        handle: SocketHandle,
    ) -> tokio::sync::watch::Receiver<bool> {
        let (write_tx, _write_rx) = mpsc::channel(64);
        let (pause_tx, pause_rx) = tokio::sync::watch::channel(false);
        let read_task = crate::util::spawn_named("test", async {});
        let write_task = crate::util::spawn_named("test", async {});
        manager.connections.insert(
            handle,
            ConnectionState::Active {
                host_read: read_task,
                host_write: write_task,
                host_write_tx: Some(write_tx),
                unsent: VecDeque::new(),
                read_paused: pause_tx,
            },
        );
        pause_rx
    }

    #[tokio::test]
    async fn handle_host_data_buffers_when_socket_cannot_send() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);

        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        insert_active(&mut manager, handle);

        manager.handle_host_event(
            TcpHostEvent::Data {
                handle,
                data: vec![1, 2, 3],
            },
            &mut sockets,
        );

        let ConnectionState::Active { unsent, .. } = manager.connections.get(&handle).unwrap()
        else {
            panic!("expected Active");
        };
        assert_eq!(unsent, &VecDeque::from(vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn handle_host_data_accumulates_in_unsent_buffer() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);

        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        insert_active(&mut manager, handle);

        manager.handle_host_event(
            TcpHostEvent::Data {
                handle,
                data: vec![1, 2],
            },
            &mut sockets,
        );
        manager.handle_host_event(
            TcpHostEvent::Data {
                handle,
                data: vec![3, 4],
            },
            &mut sockets,
        );

        let ConnectionState::Active { unsent, .. } = manager.connections.get(&handle).unwrap()
        else {
            panic!("expected Active");
        };
        assert_eq!(unsent, &VecDeque::from(vec![1, 2, 3, 4]));
    }

    #[tokio::test]
    async fn handle_host_data_pauses_reads_above_threshold() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);

        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let pause_rx = insert_active(&mut manager, handle);

        let data = vec![0u8; connection::UNSENT_PAUSE_THRESHOLD + 1];
        manager.handle_host_event(TcpHostEvent::Data { handle, data }, &mut sockets);

        assert!(
            manager.connections.get(&handle).is_some(),
            "connection should remain alive"
        );
        assert!(
            *pause_rx.borrow(),
            "read_paused should be true when unsent exceeds threshold"
        );
    }

    #[tokio::test]
    async fn handle_host_data_does_not_pause_below_threshold() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);

        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let pause_rx = insert_active(&mut manager, handle);

        manager.handle_host_event(
            TcpHostEvent::Data {
                handle,
                data: vec![0u8; 1024],
            },
            &mut sockets,
        );

        assert!(
            !*pause_rx.borrow(),
            "read_paused should remain false below threshold"
        );
    }

    #[tokio::test]
    async fn handle_host_eof_closes_smoltcp_socket() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        insert_active(&mut manager, handle);

        manager.handle_host_event(TcpHostEvent::Eof { handle }, &mut sockets);

        let socket = sockets.get_mut::<tcp::Socket>(handle);
        assert_eq!(socket.state(), tcp::State::Closed);
    }

    #[tokio::test]
    async fn handle_host_event_ignores_unknown_handle() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        manager.handle_host_event(
            TcpHostEvent::Data {
                handle,
                data: vec![1, 2, 3],
            },
            &mut sockets,
        );

        manager.handle_host_event(TcpHostEvent::Eof { handle }, &mut sockets);
    }

    #[tokio::test]
    async fn cleanup_removes_stale_pending() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let stale_time = Instant::now() - connection::TCP_CONNECT_TIMEOUT - Duration::from_secs(10);
        let task = crate::util::spawn_named("test", async {
            std::future::pending::<()>().await;
        });
        manager.connections.insert(
            handle,
            ConnectionState::Pending {
                task,
                created: stale_time,
            },
        );

        manager.cleanup(&mut sockets);

        assert!(manager.connections.is_empty());
    }

    #[tokio::test]
    async fn cleanup_keeps_fresh_pending() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let task = crate::util::spawn_named("test", async {
            std::future::pending::<()>().await;
        });
        manager.connections.insert(
            handle,
            ConnectionState::Pending {
                task,
                created: Instant::now(),
            },
        );

        manager.cleanup(&mut sockets);

        assert_eq!(manager.connections.len(), 1);
    }

    #[tokio::test]
    async fn cleanup_removes_closed_active_with_finished_tasks() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        insert_active(&mut manager, handle);

        tokio::time::sleep(Duration::from_millis(10)).await;

        manager.cleanup(&mut sockets);

        assert!(manager.connections.is_empty());
    }

    #[tokio::test]
    async fn drop_aborts_pending_tasks() {
        let (task_started_tx, mut task_started_rx) = mpsc::channel::<()>(1);

        {
            let mut manager = TcpManager::new();
            let mut sockets = SocketSet::new(vec![]);
            let socket = new_smoltcp_tcp_socket();
            let handle = sockets.add(socket);

            let task = crate::util::spawn_named("test-pending", async move {
                let _ = task_started_tx.send(()).await;
                std::future::pending::<()>().await;
            });
            manager.connections.insert(
                handle,
                ConnectionState::Pending {
                    task,
                    created: Instant::now(),
                },
            );

            task_started_rx.recv().await;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn drop_aborts_active_tasks() {
        let (read_started_tx, mut read_started_rx) = mpsc::channel::<()>(1);
        let (write_started_tx, mut write_started_rx) = mpsc::channel::<()>(1);

        {
            let mut manager = TcpManager::new();
            let mut sockets = SocketSet::new(vec![]);
            let socket = new_smoltcp_tcp_socket();
            let handle = sockets.add(socket);

            let read_task = crate::util::spawn_named("test-read", async move {
                let _ = read_started_tx.send(()).await;
                std::future::pending::<()>().await;
            });
            let write_task = crate::util::spawn_named("test-write", async move {
                let _ = write_started_tx.send(()).await;
                std::future::pending::<()>().await;
            });

            let (write_tx, _) = mpsc::channel(64);
            let (pause_tx, _) = tokio::sync::watch::channel(false);
            manager.connections.insert(
                handle,
                ConnectionState::Active {
                    host_read: read_task,
                    host_write: write_task,
                    host_write_tx: Some(write_tx),
                    unsent: VecDeque::new(),
                    read_paused: pause_tx,
                },
            );

            read_started_rx.recv().await;
            write_started_rx.recv().await;
        }

        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    #[tokio::test]
    async fn poll_sockets_skips_non_active_connections() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = TcpStream::connect(addr).await.unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();

        manager.connections.insert(
            handle,
            ConnectionState::Connected {
                stream,
                created: Instant::now(),
            },
        );

        manager.poll_sockets(&mut sockets);
    }

    #[tokio::test]
    async fn poll_connect_results_skips_non_pending() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();

        let addr_v4 = match addr {
            SocketAddr::V4(a) => a,
            _ => panic!("expected v4"),
        };

        let result = manager.initiate(socket, addr_v4, dummy_guest_src(4000), &mut sockets);
        let handle = match result {
            InitiateResult::Created(h) => h,
            _ => panic!("expected Created"),
        };
        let (_accepted, _) = listener.accept().await.unwrap();

        tokio::time::sleep(Duration::from_millis(50)).await;

        manager.poll_connect_results(&mut sockets);
        assert!(matches!(
            manager.connections.get(&handle),
            Some(ConnectionState::Connected { .. })
        ));

        let stream = TcpStream::connect(addr).await.unwrap();
        let (_accepted2, _) = listener.accept().await.unwrap();
        manager
            .connect_result_tx
            .send(ConnectResult {
                handle,
                result: Ok(stream),
            })
            .await
            .unwrap();

        manager.poll_connect_results(&mut sockets);

        assert!(matches!(
            manager.connections.get(&handle),
            Some(ConnectionState::Connected { .. })
        ));
    }

    #[tokio::test]
    async fn backpressure_on_host_write_channel_full() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let (write_tx, _write_rx) = mpsc::channel(1);
        let (pause_tx, _) = tokio::sync::watch::channel(false);
        let read_task = crate::util::spawn_named("test", async {
            std::future::pending::<()>().await;
        });
        let write_task = crate::util::spawn_named("test", async {
            std::future::pending::<()>().await;
        });
        manager.connections.insert(
            handle,
            ConnectionState::Active {
                host_read: read_task,
                host_write: write_task,
                host_write_tx: Some(write_tx),
                unsent: VecDeque::new(),
                read_paused: pause_tx,
            },
        );

        manager.poll_sockets(&mut sockets);
    }

    fn fill_manager_to_limit(manager: &mut TcpManager, sockets: &mut SocketSet<'_>) {
        for _ in 0..MAX_TCP_CONNECTIONS {
            let socket = new_smoltcp_tcp_socket();
            let handle = sockets.add(socket);
            insert_active(manager, handle);
        }
    }

    #[tokio::test]
    async fn test_initiate_at_limit_rejects_without_orphan() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);

        fill_manager_to_limit(&mut manager, &mut sockets);
        let sockets_before = sockets.iter().count();

        let socket = new_smoltcp_tcp_socket();
        let addr = SocketAddrV4::new(std::net::Ipv4Addr::new(1, 2, 3, 4), 80);
        let result = manager.initiate(socket, addr, dummy_guest_src(5000), &mut sockets);

        assert!(matches!(result, InitiateResult::RejectedLimit));
        assert_eq!(manager.connections.len(), MAX_TCP_CONNECTIONS);
        assert_eq!(sockets.iter().count(), sockets_before);
    }

    #[tokio::test]
    async fn test_register_host_stream_at_limit_rejects_without_orphan() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);

        fill_manager_to_limit(&mut manager, &mut sockets);
        let sockets_before = sockets.iter().count();

        let socket = new_smoltcp_tcp_socket();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = TcpStream::connect(addr).await.unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();

        let result = manager.register_host_stream(socket, stream, &mut sockets);

        assert!(result.is_none());
        assert_eq!(manager.connections.len(), MAX_TCP_CONNECTIONS);
        assert_eq!(sockets.iter().count(), sockets_before);
    }

    #[test]
    fn test_port_forward_connect_uses_valid_ephemeral_port() {
        let mut manager = TcpManager::new();
        let sockets = SocketSet::new(vec![]);
        let port = manager.allocate_local_port(&sockets).unwrap();
        assert!(
            (crate::config::EPHEMERAL_START..=crate::config::EPHEMERAL_END).contains(&port),
            "allocated port {} not in ephemeral range",
            port
        );
    }

    #[test]
    fn test_port_forward_connect_does_not_use_port_zero() {
        let mut manager = TcpManager::new();
        let sockets = SocketSet::new(vec![]);
        for _ in 0..100 {
            let port = manager.allocate_local_port(&sockets).unwrap();
            assert_ne!(port, 0, "allocated port must never be 0");
        }
    }

    #[tokio::test]
    async fn test_cleanup_reaps_stale_connected() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = TcpStream::connect(addr).await.unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();

        let stale_time =
            Instant::now() - connection::CONNECTED_HANDSHAKE_TIMEOUT - Duration::from_secs(5);
        manager.connections.insert(
            handle,
            ConnectionState::Connected {
                stream,
                created: stale_time,
            },
        );

        manager.cleanup(&mut sockets);

        assert!(manager.connections.is_empty());
        assert_eq!(
            sockets.iter().count(),
            0,
            "socket should be removed from SocketSet"
        );
    }

    #[tokio::test]
    async fn test_cleanup_keeps_fresh_connected() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let stream = TcpStream::connect(addr).await.unwrap();
        let (_accepted, _) = listener.accept().await.unwrap();

        manager.connections.insert(
            handle,
            ConnectionState::Connected {
                stream,
                created: Instant::now(),
            },
        );

        manager.cleanup(&mut sockets);

        assert_eq!(manager.connections.len(), 1);
        assert_eq!(
            sockets.iter().count(),
            1,
            "socket should remain in SocketSet"
        );
    }

    #[tokio::test]
    async fn test_initiate_rejects_duplicate_flow() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);

        let guest_src = dummy_guest_src(6000);
        let remote_dst = SocketAddrV4::new(std::net::Ipv4Addr::new(93, 184, 216, 34), 80);

        let socket1 = new_smoltcp_tcp_socket();
        let result1 = manager.initiate(socket1, remote_dst, guest_src, &mut sockets);
        assert!(matches!(result1, InitiateResult::Created(_)));
        assert_eq!(manager.connections.len(), 1);

        let socket2 = new_smoltcp_tcp_socket();
        let result2 = manager.initiate(socket2, remote_dst, guest_src, &mut sockets);
        assert!(matches!(result2, InitiateResult::DuplicateFlow));
        assert_eq!(manager.connections.len(), 1);
        assert_eq!(sockets.iter().count(), 1);
    }

    #[tokio::test]
    async fn test_initiate_allows_different_flows() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);

        let remote_dst = SocketAddrV4::new(std::net::Ipv4Addr::new(93, 184, 216, 34), 80);

        let socket1 = new_smoltcp_tcp_socket();
        let result1 = manager.initiate(socket1, remote_dst, dummy_guest_src(7000), &mut sockets);
        assert!(matches!(result1, InitiateResult::Created(_)));

        let socket2 = new_smoltcp_tcp_socket();
        let result2 = manager.initiate(socket2, remote_dst, dummy_guest_src(7001), &mut sockets);
        assert!(matches!(result2, InitiateResult::Created(_)));

        assert_eq!(manager.connections.len(), 2);
        assert_eq!(manager.flow_index.len(), 2);
    }

    #[tokio::test]
    async fn test_flow_index_cleaned_on_connect_failure() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);

        let guest_src = dummy_guest_src(8000);
        let unreachable = SocketAddrV4::new(std::net::Ipv4Addr::new(127, 0, 0, 1), 1);

        let socket = new_smoltcp_tcp_socket();
        let result = manager.initiate(socket, unreachable, guest_src, &mut sockets);
        assert!(matches!(result, InitiateResult::Created(_)));
        assert_eq!(manager.flow_index.len(), 1);

        tokio::time::sleep(Duration::from_millis(200)).await;

        manager.poll_connect_results(&mut sockets);

        assert!(manager.connections.is_empty());
        assert!(manager.flow_index.is_empty());
    }

    #[tokio::test]
    async fn test_flow_index_cleaned_on_pending_timeout() {
        let mut manager = TcpManager::new();
        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let guest_src = dummy_guest_src(9000);
        let remote_dst = SocketAddrV4::new(std::net::Ipv4Addr::new(93, 184, 216, 34), 80);

        let flow_key = FlowKey {
            guest_src,
            remote_dst,
        };
        manager.flow_index.insert(flow_key, handle);

        let stale_time = Instant::now() - connection::TCP_CONNECT_TIMEOUT - Duration::from_secs(10);
        let task = crate::util::spawn_named("test", async {
            std::future::pending::<()>().await;
        });
        manager.connections.insert(
            handle,
            ConnectionState::Pending {
                task,
                created: stale_time,
            },
        );

        manager.cleanup(&mut sockets);

        assert!(manager.connections.is_empty());
        assert!(manager.flow_index.is_empty());
    }
}
