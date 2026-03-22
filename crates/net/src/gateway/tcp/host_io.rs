use smoltcp::iface::SocketHandle;
use smoltcp::socket::tcp;
use std::collections::VecDeque;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::mpsc;

use super::connection::HOST_READ_BUF;

pub enum TcpHostEvent {
    Data { handle: SocketHandle, data: Vec<u8> },
    Eof { handle: SocketHandle },
}

pub(crate) async fn host_read_task(
    mut read_half: tokio::net::tcp::OwnedReadHalf,
    tx: mpsc::Sender<TcpHostEvent>,
    handle: SocketHandle,
    mut pause_rx: tokio::sync::watch::Receiver<bool>,
) {
    let mut buf = vec![0u8; HOST_READ_BUF];
    loop {
        while *pause_rx.borrow_and_update() {
            if pause_rx.changed().await.is_err() {
                return;
            }
        }
        match read_half.read(&mut buf).await {
            Ok(0) => {
                let _ = tx.send(TcpHostEvent::Eof { handle }).await;
                break;
            }
            Ok(n) => {
                let data = buf[..n].to_vec();
                if tx.send(TcpHostEvent::Data { handle, data }).await.is_err() {
                    break;
                }
            }
            Err(_) => {
                let _ = tx.send(TcpHostEvent::Eof { handle }).await;
                break;
            }
        }
    }
}

pub(crate) async fn host_write_task(
    mut write_half: tokio::net::tcp::OwnedWriteHalf,
    mut rx: mpsc::Receiver<Vec<u8>>,
) {
    while let Some(data) = rx.recv().await {
        if write_half.write_all(&data).await.is_err() {
            break;
        }
    }
    let _ = write_half.shutdown().await;
}

pub(super) fn drain_unsent(socket: &mut tcp::Socket<'_>, unsent: &mut VecDeque<u8>) {
    if unsent.is_empty() || !socket.can_send() {
        return;
    }
    let (front, back) = unsent.as_slices();
    let mut total_sent = 0;
    match socket.send_slice(front) {
        Ok(sent) => {
            total_sent += sent;
            if sent == front.len() && !back.is_empty() {
                match socket.send_slice(back) {
                    Ok(sent2) => total_sent += sent2,
                    Err(e) => {
                        tracing::debug!("TCP manager: send_slice failed: {}", e);
                    }
                }
            }
        }
        Err(e) => {
            tracing::debug!("TCP manager: send_slice failed: {}", e);
        }
    }
    if total_sent > 0 {
        unsent.drain(..total_sent);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use smoltcp::iface::SocketSet;
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;
    use tokio::net::TcpStream;

    fn new_smoltcp_tcp_socket() -> tcp::Socket<'static> {
        super::super::new_smoltcp_tcp_socket()
    }

    #[tokio::test]
    async fn host_read_task_sends_data_and_eof() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (event_tx, mut event_rx) = mpsc::channel(16);

        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let client = TcpStream::connect(addr).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();

        let (read_half, _write_half) = client.into_split();

        let (_pause_tx, pause_rx) = tokio::sync::watch::channel(false);
        let _task = crate::util::spawn_named("test-host-read", async move {
            host_read_task(read_half, event_tx, handle, pause_rx).await;
        });

        server.write_all(b"hello world").await.unwrap();
        server.shutdown().await.unwrap();
        drop(server);

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            TcpHostEvent::Data { data, .. } => {
                assert_eq!(&data, b"hello world");
            }
            TcpHostEvent::Eof { .. } => panic!("expected Data, got Eof"),
        }

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event, TcpHostEvent::Eof { .. }));
    }

    #[tokio::test]
    async fn host_write_task_writes_data_and_shuts_down() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let client = TcpStream::connect(addr).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();

        let (_read_half, write_half) = client.into_split();
        let (data_tx, data_rx) = mpsc::channel::<Vec<u8>>(64);

        let _task = crate::util::spawn_named("test-host-write", async move {
            host_write_task(write_half, data_rx).await;
        });

        data_tx.send(b"test data".to_vec()).await.unwrap();
        drop(data_tx);

        let mut buf = vec![0u8; 1024];
        let n = tokio::time::timeout(Duration::from_secs(2), server.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(&buf[..n], b"test data");

        let n = tokio::time::timeout(Duration::from_secs(2), server.read(&mut buf))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(n, 0);
    }

    #[test]
    fn drain_unsent_does_nothing_when_empty() {
        let mut sockets = SocketSet::new(vec![]);
        let handle = sockets.add(new_smoltcp_tcp_socket());
        let socket = sockets.get_mut::<tcp::Socket>(handle);

        let mut unsent = std::collections::VecDeque::new();
        drain_unsent(socket, &mut unsent);
        assert!(unsent.is_empty());
    }

    #[test]
    fn drain_unsent_keeps_data_when_socket_cannot_send() {
        let mut sockets = SocketSet::new(vec![]);
        let handle = sockets.add(new_smoltcp_tcp_socket());
        let socket = sockets.get_mut::<tcp::Socket>(handle);

        assert!(!socket.can_send());

        let mut unsent = std::collections::VecDeque::from(vec![1, 2, 3]);
        drain_unsent(socket, &mut unsent);
        assert_eq!(unsent, std::collections::VecDeque::from(vec![1, 2, 3]));
    }

    #[tokio::test]
    async fn host_read_task_pauses_when_signaled() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();

        let (event_tx, mut event_rx) = mpsc::channel(16);

        let mut sockets = SocketSet::new(vec![]);
        let socket = new_smoltcp_tcp_socket();
        let handle = sockets.add(socket);

        let client = TcpStream::connect(addr).await.unwrap();
        let (mut server, _) = listener.accept().await.unwrap();

        let (read_half, _write_half) = client.into_split();

        let (pause_tx, pause_rx) = tokio::sync::watch::channel(true);
        let _task = crate::util::spawn_named("test-host-read", async move {
            host_read_task(read_half, event_tx, handle, pause_rx).await;
        });

        server.write_all(b"hello").await.unwrap();

        let result = tokio::time::timeout(Duration::from_millis(100), event_rx.recv()).await;
        assert!(result.is_err(), "should not receive data while paused");

        pause_tx.send(false).unwrap();

        let event = tokio::time::timeout(Duration::from_secs(2), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        match event {
            TcpHostEvent::Data { data, .. } => assert_eq!(&data, b"hello"),
            TcpHostEvent::Eof { .. } => panic!("expected Data, got Eof"),
        }
    }
}
