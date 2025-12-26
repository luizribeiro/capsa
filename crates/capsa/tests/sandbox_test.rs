//! Integration tests for the sandbox VM and agent.
//!
//! These tests verify the sandbox functionality including:
//! - Agent RPC communication via vsock
//! - Command execution in the guest
//! - File operations (read, write, list, exists)
//! - System information retrieval

use capsa::Capsa;
use capsa::sandbox;
use std::collections::HashMap;
use std::time::Duration;
use tempfile::TempDir;

/// Tests that the sandbox agent responds to ping.
#[tokio::test]
async fn test_sandbox_agent_ping() {
    let vm = Capsa::sandbox()
        .run("/bin/sh", &["-c", "sleep infinity"])
        .build()
        .await
        .expect("Failed to build sandbox VM");

    let socket = vm
        .vsock_socket(sandbox::agent_port())
        .expect("vsock socket for agent not found");

    let agent = sandbox::wait_ready(socket)
        .await
        .expect("Agent did not become ready");

    agent.ping().await.expect("Ping failed");

    vm.kill().await.expect("Failed to kill VM");
}

/// Tests command execution in the sandbox.
#[tokio::test]
async fn test_sandbox_exec() {
    let vm = Capsa::sandbox()
        .run("/bin/sh", &["-c", "sleep infinity"])
        .build()
        .await
        .expect("Failed to build sandbox VM");

    let socket = vm
        .vsock_socket(sandbox::agent_port())
        .expect("vsock socket not found");

    let agent = sandbox::wait_ready(socket).await.expect("Agent not ready");

    // Test simple command
    let result = agent
        .exec("echo hello", HashMap::new())
        .await
        .expect("exec failed");
    assert_eq!(result.stdout.trim(), "hello");
    assert_eq!(result.exit_code, 0);

    // Test command with exit code
    let result = agent
        .exec("exit 42", HashMap::new())
        .await
        .expect("exec failed");
    assert_eq!(result.exit_code, 42);

    // Test command with environment variables
    let mut env = HashMap::new();
    env.insert("MY_VAR".to_string(), "my_value".to_string());
    let result = agent.exec("echo $MY_VAR", env).await.expect("exec failed");
    assert_eq!(result.stdout.trim(), "my_value");

    vm.kill().await.expect("Failed to kill VM");
}

/// Tests file read/write operations in the sandbox.
#[tokio::test]
async fn test_sandbox_file_operations() {
    let vm = Capsa::sandbox()
        .run("/bin/sh", &["-c", "sleep infinity"])
        .build()
        .await
        .expect("Failed to build sandbox VM");

    let socket = vm
        .vsock_socket(sandbox::agent_port())
        .expect("vsock socket not found");

    let agent = sandbox::wait_ready(socket).await.expect("Agent not ready");

    // Write a file
    let test_content = b"Hello from host!";
    agent
        .write_file("/tmp/test.txt", test_content)
        .await
        .expect("write_file failed");

    // Read it back
    let content = agent
        .read_file("/tmp/test.txt")
        .await
        .expect("read_file failed");
    assert_eq!(content, test_content);

    // Check exists
    assert!(agent.exists("/tmp/test.txt").await.expect("exists failed"));
    assert!(
        !agent
            .exists("/tmp/nonexistent.txt")
            .await
            .expect("exists failed")
    );

    // List directory
    let entries = agent.list_dir("/tmp").await.expect("list_dir failed");
    assert!(entries.iter().any(|e| e.name == "test.txt"));

    vm.kill().await.expect("Failed to kill VM");
}

/// Tests system info retrieval from the sandbox.
#[tokio::test]
async fn test_sandbox_info() {
    let vm = Capsa::sandbox()
        .run("/bin/sh", &["-c", "sleep infinity"])
        .build()
        .await
        .expect("Failed to build sandbox VM");

    let socket = vm
        .vsock_socket(sandbox::agent_port())
        .expect("vsock socket not found");

    let agent = sandbox::wait_ready(socket).await.expect("Agent not ready");

    let info = agent.info().await.expect("info failed");

    // Verify we get reasonable values
    assert!(!info.kernel_version.is_empty());
    assert!(info.cpus > 0);
    assert!(info.memory_bytes > 0);
    assert!(!info.mounts.is_empty());

    vm.kill().await.expect("Failed to kill VM");
}

/// Tests shared directory mounting in the sandbox.
#[tokio::test]
async fn test_sandbox_shared_directory() {
    use capsa::MountMode;

    let tmp_dir = TempDir::new().expect("Failed to create temp dir");
    let test_file = tmp_dir.path().join("shared.txt");
    std::fs::write(&test_file, "Content from host").expect("Failed to write test file");

    let vm = Capsa::sandbox()
        .share(tmp_dir.path(), "/mnt/share", MountMode::ReadOnly)
        .run("/bin/sh", &["-c", "sleep infinity"])
        .build()
        .await
        .expect("Failed to build sandbox VM");

    let socket = vm
        .vsock_socket(sandbox::agent_port())
        .expect("vsock socket not found");

    let agent = sandbox::wait_ready(socket).await.expect("Agent not ready");

    // Wait a moment for mounts to complete
    tokio::time::sleep(Duration::from_millis(500)).await;

    // Read the shared file from inside the sandbox
    let content = agent
        .read_file("/mnt/share/shared.txt")
        .await
        .expect("Failed to read shared file");
    assert_eq!(String::from_utf8_lossy(&content), "Content from host");

    vm.kill().await.expect("Failed to kill VM");
}

/// Tests agent shutdown request.
#[tokio::test]
async fn test_sandbox_shutdown() {
    let vm = Capsa::sandbox()
        .run("/bin/sh", &["-c", "sleep infinity"])
        .build()
        .await
        .expect("Failed to build sandbox VM");

    let socket = vm
        .vsock_socket(sandbox::agent_port())
        .expect("vsock socket not found");

    let agent = sandbox::wait_ready(socket).await.expect("Agent not ready");

    // Request shutdown
    agent.shutdown().await.expect("shutdown failed");

    // Give the VM time to shut down
    tokio::time::sleep(Duration::from_millis(500)).await;

    // The VM should have exited - cleanup
    let _ = vm.kill().await;
}
