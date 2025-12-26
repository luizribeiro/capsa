//! Integration tests for shared directories (virtio-fs).
//!
//! These tests verify that host directories can be shared with guest VMs
//! using virtio-fs, including both read-only and read-write operations.

use capsa::MountMode;
use capsa::test_utils::test_vm;
use std::fs;
use std::time::Duration;
use tempfile::TempDir;

/// Helper to check if the current backend supports virtio-fs.
fn backend_supports_virtiofs() -> bool {
    #[cfg(feature = "linux-kvm")]
    {
        true
    }
    #[cfg(not(feature = "linux-kvm"))]
    {
        false
    }
}

/// Tests mounting a read-only shared directory and reading files.
#[tokio::test]
async fn test_virtiofs_read_only() {
    if !backend_supports_virtiofs() {
        eprintln!("Skipping: backend does not support virtio-fs");
        return;
    }

    // Create a temporary directory with test files
    let tmp_dir = TempDir::new().expect("Failed to create temp dir");
    let test_file = tmp_dir.path().join("hello.txt");
    fs::write(&test_file, "Hello from host!").expect("Failed to write test file");

    let nested_dir = tmp_dir.path().join("subdir");
    fs::create_dir(&nested_dir).expect("Failed to create subdir");
    fs::write(nested_dir.join("nested.txt"), "Nested content")
        .expect("Failed to write nested file");

    // Start VM with shared directory
    let vm = test_vm("default")
        .share(tmp_dir.path(), "share0", MountMode::ReadOnly)
        .build()
        .await
        .expect("Failed to build VM");

    let console = vm.console().await.expect("Failed to get console");

    // Wait for boot
    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    // Mount the virtio-fs share
    let output = console
        .exec(
            "mkdir -p /mnt/share && mount -t virtiofs share0 /mnt/share && echo MOUNT_OK",
            Duration::from_secs(10),
        )
        .await
        .expect("Failed to mount share");
    assert!(
        output.contains("MOUNT_OK"),
        "Mount failed, output: {}",
        output
    );

    // Read the test file
    let output = console
        .exec("cat /mnt/share/hello.txt", Duration::from_secs(5))
        .await
        .expect("Failed to read file");
    assert!(
        output.contains("Hello from host!"),
        "File content mismatch: {}",
        output
    );

    // List directory contents
    let output = console
        .exec("ls /mnt/share", Duration::from_secs(5))
        .await
        .expect("Failed to list directory");
    assert!(
        output.contains("hello.txt"),
        "Missing hello.txt: {}",
        output
    );
    assert!(output.contains("subdir"), "Missing subdir: {}", output);

    // Read nested file
    let output = console
        .exec("cat /mnt/share/subdir/nested.txt", Duration::from_secs(5))
        .await
        .expect("Failed to read nested file");
    assert!(
        output.contains("Nested content"),
        "Nested file content mismatch: {}",
        output
    );

    // Verify read-only: writing should fail
    let output = console
        .exec(
            "echo 'test' > /mnt/share/newfile.txt 2>&1 || echo WRITE_FAILED",
            Duration::from_secs(5),
        )
        .await
        .expect("Failed to test write");
    assert!(
        output.contains("WRITE_FAILED") || output.contains("Read-only"),
        "Write should have failed on read-only share: {}",
        output
    );

    vm.kill().await.expect("Failed to kill VM");
}

/// Tests mounting a read-write shared directory and writing files.
#[tokio::test]
async fn test_virtiofs_read_write() {
    if !backend_supports_virtiofs() {
        eprintln!("Skipping: backend does not support virtio-fs");
        return;
    }

    // Create a temporary directory
    let tmp_dir = TempDir::new().expect("Failed to create temp dir");
    let existing_file = tmp_dir.path().join("existing.txt");
    fs::write(&existing_file, "Original content").expect("Failed to write file");

    // Start VM with read-write share
    let vm = test_vm("default")
        .share(tmp_dir.path(), "share0", MountMode::ReadWrite)
        .build()
        .await
        .expect("Failed to build VM");

    let console = vm.console().await.expect("Failed to get console");

    // Wait for boot
    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    // Mount the share
    let output = console
        .exec(
            "mkdir -p /mnt/share && mount -t virtiofs share0 /mnt/share && echo MOUNT_OK",
            Duration::from_secs(10),
        )
        .await
        .expect("Failed to mount share");
    assert!(
        output.contains("MOUNT_OK"),
        "Mount failed, output: {}",
        output
    );

    // Read existing file
    let output = console
        .exec("cat /mnt/share/existing.txt", Duration::from_secs(5))
        .await
        .expect("Failed to read existing file");
    assert!(
        output.contains("Original content"),
        "Existing file content mismatch: {}",
        output
    );

    // Create a new file from guest
    let output = console
        .exec(
            "echo 'Created by guest' > /mnt/share/guest_file.txt && echo WRITE_OK",
            Duration::from_secs(5),
        )
        .await
        .expect("Failed to create file");
    assert!(
        output.contains("WRITE_OK"),
        "File creation failed: {}",
        output
    );

    // Create a directory from guest
    let output = console
        .exec(
            "mkdir /mnt/share/guest_dir && echo 'nested' > /mnt/share/guest_dir/file.txt && echo DIR_OK",
            Duration::from_secs(5),
        )
        .await
        .expect("Failed to create directory");
    assert!(
        output.contains("DIR_OK"),
        "Directory creation failed: {}",
        output
    );

    // Modify existing file
    let output = console
        .exec(
            "echo 'Modified by guest' > /mnt/share/existing.txt && echo MODIFY_OK",
            Duration::from_secs(5),
        )
        .await
        .expect("Failed to modify file");
    assert!(
        output.contains("MODIFY_OK"),
        "File modification failed: {}",
        output
    );

    vm.kill().await.expect("Failed to kill VM");

    // Verify changes are visible on host
    let guest_file = tmp_dir.path().join("guest_file.txt");
    assert!(guest_file.exists(), "Guest-created file not found on host");
    let content = fs::read_to_string(&guest_file).expect("Failed to read guest file");
    assert!(
        content.contains("Created by guest"),
        "Guest file content mismatch: {}",
        content
    );

    let guest_dir = tmp_dir.path().join("guest_dir");
    assert!(guest_dir.is_dir(), "Guest-created directory not found");
    let nested_file = guest_dir.join("file.txt");
    assert!(nested_file.exists(), "Nested file not found");

    let modified = fs::read_to_string(&existing_file).expect("Failed to read modified file");
    assert!(
        modified.contains("Modified by guest"),
        "File modification not persisted: {}",
        modified
    );
}

/// Tests multiple shared directories in a single VM.
#[tokio::test]
async fn test_virtiofs_multiple_shares() {
    if !backend_supports_virtiofs() {
        eprintln!("Skipping: backend does not support virtio-fs");
        return;
    }

    let tmp_dir1 = TempDir::new().expect("Failed to create temp dir 1");
    let tmp_dir2 = TempDir::new().expect("Failed to create temp dir 2");

    fs::write(tmp_dir1.path().join("file1.txt"), "Content 1").expect("Failed to write file 1");
    fs::write(tmp_dir2.path().join("file2.txt"), "Content 2").expect("Failed to write file 2");

    let vm = test_vm("default")
        .share(tmp_dir1.path(), "share0", MountMode::ReadOnly)
        .share(tmp_dir2.path(), "share1", MountMode::ReadOnly)
        .build()
        .await
        .expect("Failed to build VM");

    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    // Mount first share
    let output = console
        .exec(
            "mkdir -p /mnt/s0 && mount -t virtiofs share0 /mnt/s0 && echo MOUNT0_OK",
            Duration::from_secs(10),
        )
        .await
        .expect("Failed to mount share0");
    assert!(
        output.contains("MOUNT0_OK"),
        "Mount share0 failed: {}",
        output
    );

    // Mount second share
    let output = console
        .exec(
            "mkdir -p /mnt/s1 && mount -t virtiofs share1 /mnt/s1 && echo MOUNT1_OK",
            Duration::from_secs(10),
        )
        .await
        .expect("Failed to mount share1");
    assert!(
        output.contains("MOUNT1_OK"),
        "Mount share1 failed: {}",
        output
    );

    // Read from both shares
    let output = console
        .exec("cat /mnt/s0/file1.txt", Duration::from_secs(5))
        .await
        .expect("Failed to read from share0");
    assert!(
        output.contains("Content 1"),
        "Share0 content mismatch: {}",
        output
    );

    let output = console
        .exec("cat /mnt/s1/file2.txt", Duration::from_secs(5))
        .await
        .expect("Failed to read from share1");
    assert!(
        output.contains("Content 2"),
        "Share1 content mismatch: {}",
        output
    );

    vm.kill().await.expect("Failed to kill VM");
}

/// Tests that path traversal attacks are blocked.
#[tokio::test]
async fn test_virtiofs_path_traversal_blocked() {
    if !backend_supports_virtiofs() {
        eprintln!("Skipping: backend does not support virtio-fs");
        return;
    }

    let tmp_dir = TempDir::new().expect("Failed to create temp dir");
    fs::write(tmp_dir.path().join("safe.txt"), "Safe content").expect("Failed to write file");

    // Create a file outside the share that we'll try to access
    let parent_file = tmp_dir.path().parent().unwrap().join("outside.txt");
    let _ = fs::write(&parent_file, "Outside content");

    let vm = test_vm("default")
        .share(tmp_dir.path(), "share0", MountMode::ReadOnly)
        .build()
        .await
        .expect("Failed to build VM");

    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    console
        .exec(
            "mkdir -p /mnt/share && mount -t virtiofs share0 /mnt/share",
            Duration::from_secs(10),
        )
        .await
        .expect("Failed to mount share");

    // Try to read safe file (should work)
    let output = console
        .exec("cat /mnt/share/safe.txt", Duration::from_secs(5))
        .await
        .expect("Failed to read safe file");
    assert!(
        output.contains("Safe content"),
        "Safe file should be readable: {}",
        output
    );

    // Try path traversal (should fail)
    let output = console
        .exec(
            "cat /mnt/share/../outside.txt 2>&1 || echo ACCESS_DENIED",
            Duration::from_secs(5),
        )
        .await
        .expect("Failed to test path traversal");

    // The path traversal should either fail or return the safe file (if resolved within share)
    // It should NOT return "Outside content"
    assert!(
        !output.contains("Outside content"),
        "Path traversal should be blocked! Got: {}",
        output
    );

    vm.kill().await.expect("Failed to kill VM");
}
