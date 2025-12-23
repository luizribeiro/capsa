#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for vsock (VM socket) functionality.
//!
//! These tests require the vsock test VM which includes a ping-pong
//! responder program. The guest program connects to the host via vsock
//! and responds to "ping" messages with "pong".
//!
//! NOTE: These tests work with the macos-native backend. The vfkit backend
//! has a bug in v0.6.1 where the vsock Unix socket is never created.

#[cfg(not(feature = "linux-kvm"))]
use capsa::test_utils::test_vm;
#[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
use std::time::Duration;
#[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Tests vsock ping-pong communication between host and guest.
#[apple_main::harness_test]
async fn test_vsock_ping_pong() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support vsock yet");
        return;
    }

    #[cfg(feature = "vfkit")]
    {
        eprintln!("Skipping: vfkit v0.6.1 has a bug where vsock Unix socket is never created");
        return;
    }

    #[cfg(not(any(feature = "linux-kvm", feature = "vfkit")))]
    {
        // Start VM with vsock configured on port 1024
        let vm = test_vm("default")
            .vsock_listen(1024)
            .build()
            .await
            .expect("Failed to build VM");

        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot and vsock-pong to start
        console
            .wait_for_timeout("vsock-pong started", Duration::from_secs(30))
            .await
            .expect("VM did not start vsock-pong");

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
}

#[apple_main::harness_test]
async fn test_vsock_socket_info() {
    // TODO: KVM backend doesn't support vsock yet
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping test_vsock_socket_info on KVM backend (vsock not yet implemented)");
        return;
    }

    #[cfg(not(feature = "linux-kvm"))]
    {
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
}

#[apple_main::harness_test]
async fn test_vsock_multiple_ports() {
    // TODO: KVM backend doesn't support vsock yet
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping test_vsock_multiple_ports on KVM backend (vsock not yet implemented)");
        return;
    }

    #[cfg(not(feature = "linux-kvm"))]
    {
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
}
