//! Integration tests for vsock (VM socket) functionality.
//!
//! These tests require the vsock test VM which includes a ping-pong
//! responder program. The guest program connects to the host via vsock
//! and responds to "ping" messages with "pong".

use capsa::test_utils::test_vm;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Tests vsock ping-pong communication between host and guest.
#[tokio::test]
async fn test_vsock_ping_pong() {
    // Start VM with vsock configured on port 1024
    let vm = test_vm("default")
        .vsock_listen(1024)
        .build()
        .await
        .expect("Failed to build VM");

    let console = vm.console().await.expect("Failed to get console");

    // Wait for boot then start vsock-pong
    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    console
        .exec("/bin/vsock-pong connect 1024 &", Duration::from_secs(5))
        .await
        .expect("Failed to start vsock-pong");

    // Get the vsock socket
    let socket = vm
        .vsock_socket(1024)
        .expect("vsock socket for port 1024 not found");

    // Connect to the vsock socket
    let mut stream = socket
        .connect()
        .await
        .expect("Failed to connect to vsock socket");

    // Send ping
    stream
        .write_all(b"ping")
        .await
        .expect("Failed to write ping");

    // Read pong response
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("Timeout waiting for pong")
        .expect("Failed to read pong");

    let response = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(
        response, "pong",
        "Expected 'pong' response, got '{}'",
        response
    );

    // Send quit to cleanly close the connection
    let _ = stream.write_all(b"quit").await;

    vm.kill().await.expect("Failed to kill VM");
}

#[tokio::test]
async fn test_vsock_socket_info() {
    // Test that VsockSocket provides correct port and path info
    let vm = test_vm("default")
        .vsock_listen(2048)
        .build()
        .await
        .expect("Failed to build VM");

    let socket = vm
        .vsock_socket(2048)
        .expect("vsock socket for port 2048 not found");

    assert_eq!(socket.port(), 2048);
    assert!(socket.path().to_string_lossy().contains("capsa"));
    assert!(socket.path().to_string_lossy().contains("2048"));

    // Verify unconfigured port returns None
    assert!(vm.vsock_socket(9999).is_none());

    vm.kill().await.expect("Failed to kill VM");
}

#[tokio::test]
async fn test_vsock_multiple_ports() {
    // Test configuring multiple vsock ports
    let vm = test_vm("default")
        .vsock_listen(1024)
        .vsock_listen(1025)
        .build()
        .await
        .expect("Failed to build VM");

    let sockets = vm.vsock_sockets();
    assert_eq!(sockets.len(), 2);
    assert!(sockets.contains_key(&1024));
    assert!(sockets.contains_key(&1025));

    vm.kill().await.expect("Failed to kill VM");
}

/// Tests host-to-guest vsock connection (connect mode).
/// In this mode, the guest listens and the host initiates the connection.
#[tokio::test]
async fn test_vsock_host_to_guest() {
    // Start VM with vsock connect mode on port 2049
    let vm = test_vm("default")
        .vsock_connect(2049)
        .build()
        .await
        .expect("Failed to build VM");

    let console = vm.console().await.expect("Failed to get console");

    // Wait for boot
    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    // Start vsock-pong in listen mode (guest will listen for connections)
    console
        .exec("/bin/vsock-pong listen 2049 &", Duration::from_secs(5))
        .await
        .expect("Failed to start vsock-pong in listen mode");

    // Give the guest time to start listening
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Get the vsock socket and connect (host initiates connection to guest)
    let socket = vm
        .vsock_socket(2049)
        .expect("vsock socket for port 2049 not found");

    let mut stream = socket
        .connect()
        .await
        .expect("Failed to connect to vsock socket");

    // Send ping
    stream
        .write_all(b"ping")
        .await
        .expect("Failed to write ping");

    // Read pong response
    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("Timeout waiting for pong")
        .expect("Failed to read pong");

    let response = String::from_utf8_lossy(&buf[..n]);
    assert_eq!(
        response, "pong",
        "Expected 'pong' response, got '{}'",
        response
    );

    // Send quit to cleanly close the connection
    let _ = stream.write_all(b"quit").await;

    vm.kill().await.expect("Failed to kill VM");
}
