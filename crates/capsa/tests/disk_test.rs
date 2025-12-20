#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for VM disk functionality.

use capsa::test_utils::{test_vm, vm_paths};
use capsa::{Capsa, DiskImage, LinuxDirectBootConfig};
use std::time::Duration;

#[apple_main::harness_test]
async fn test_vm_with_readonly_disk_mounts() {
    // Uses read-only disk (default for test VMs since disk is in Nix store)
    let vm = test_vm("with-disk")
        .build()
        .await
        .expect("Failed to build VM");
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    console
        .wait_for_timeout("Disk mounted at /mnt", Duration::from_secs(10))
        .await
        .expect("Disk was not mounted");

    // Verify it's mounted read-only
    console
        .wait_for_timeout("read-only", Duration::from_secs(5))
        .await
        .expect("Disk should be mounted read-only");

    vm.kill().await.expect("Failed to kill VM");
}

#[apple_main::harness_test]
async fn test_disk_read_write() {
    // Copy disk to temp file so we can write to it
    let paths = vm_paths("with-disk");
    let disk_path = paths.disk.as_ref().expect("with-disk should have a disk");

    let temp_disk = tempfile::NamedTempFile::new().expect("Failed to create temp file");
    std::fs::copy(disk_path, temp_disk.path()).expect("Failed to copy disk");

    let config = LinuxDirectBootConfig::new(&paths.kernel, &paths.initrd)
        .with_root_disk(DiskImage::new(temp_disk.path()));

    let vm = Capsa::linux(config)
        .console_enabled()
        .build()
        .await
        .expect("Failed to build VM");
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Disk mounted at /mnt", Duration::from_secs(30))
        .await
        .expect("Disk was not mounted");

    // Verify it's mounted read-write
    console
        .wait_for_timeout("read-write", Duration::from_secs(5))
        .await
        .expect("Disk should be mounted read-write");

    tokio::time::sleep(Duration::from_millis(50)).await;

    console
        .write_line("echo 'test-content-12345' > /mnt/test-file.txt")
        .await
        .expect("Failed to write file");

    tokio::time::sleep(Duration::from_millis(50)).await;

    console
        .write_line("cat /mnt/test-file.txt")
        .await
        .expect("Failed to read file");

    console
        .wait_for_timeout("test-content-12345", Duration::from_secs(5))
        .await
        .expect("File content not found");

    vm.kill().await.expect("Failed to kill VM");
}
