#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for VM disk functionality.

use capsa::test_utils::test_vm;
use std::time::Duration;

#[apple_main::harness_test]
async fn test_vm_with_disk_mounts_successfully() {
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

    vm.kill().await.expect("Failed to kill VM");
}

#[apple_main::harness_test]
async fn test_disk_read_write() {
    let vm = test_vm("with-disk")
        .build()
        .await
        .expect("Failed to build VM");
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Disk mounted at /mnt", Duration::from_secs(30))
        .await
        .expect("Disk was not mounted");

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
