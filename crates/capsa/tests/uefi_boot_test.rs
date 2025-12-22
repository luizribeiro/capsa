#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for UEFI boot functionality.
//!
//! These tests use vsock ping-pong to verify boot success because console output
//! doesn't work reliably with UEFI boot on Apple Virtualization.framework.

use capsa::test_utils::test_uefi_vm;
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

const VSOCK_PORT: u32 = 1024;

async fn verify_vm_booted_via_console_and_vsock(vm: &capsa::VmHandle) {
    // With ACPI enabled, console now works with UEFI boot!
    // Use console to wait for boot, then verify vsock
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("vsock-pong started", Duration::from_secs(30))
        .await
        .expect("UEFI VM did not start vsock-pong");

    // Now verify vsock works
    let socket = vm
        .vsock_socket(VSOCK_PORT)
        .expect("vsock socket for port 1024 not found");

    let mut stream = socket.connect().await.expect("Failed to connect to vsock");

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
}

#[apple_main::harness_test]
async fn test_uefi_vm_boots_successfully() {
    let vm = test_uefi_vm("uefi")
        .vsock_listen(VSOCK_PORT)
        .build()
        .await
        .expect("Failed to build UEFI VM");

    verify_vm_booted_via_console_and_vsock(&vm).await;

    vm.kill().await.expect("Failed to stop UEFI VM");
}

#[apple_main::harness_test]
async fn test_uefi_kernel_via_linux_boot() {
    use capsa::test_utils::test_vm;

    // Boot the UEFI VM's kernel/initrd via Linux direct boot to verify vsock works
    let vm = test_vm("uefi")
        .vsock_listen(VSOCK_PORT)
        .build()
        .await
        .expect("Failed to build VM with UEFI kernel via Linux boot");

    let console = vm.console().await.expect("Failed to get console");

    // Wait for vsock-pong to start via console
    console
        .wait_for_timeout("vsock-pong started", Duration::from_secs(30))
        .await
        .expect("VM did not start vsock-pong");

    // Now do ping-pong
    let socket = vm.vsock_socket(VSOCK_PORT).expect("vsock socket not found");

    let mut stream = socket.connect().await.expect("Failed to connect");
    stream.write_all(b"ping").await.expect("Failed to write");

    let mut buf = [0u8; 16];
    let n = tokio::time::timeout(Duration::from_secs(5), stream.read(&mut buf))
        .await
        .expect("Timeout")
        .expect("Failed to read");

    assert_eq!(&buf[..n], b"pong");
    let _ = stream.write_all(b"quit").await;

    vm.kill().await.expect("Failed to stop VM");
}
