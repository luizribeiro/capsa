#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for UEFI boot functionality.

use capsa::test_utils::{UEFI_BOOT_SUCCESS_MESSAGE, test_uefi_pool, test_uefi_vm};
use std::time::Duration;

#[apple_main::harness_test]
async fn test_uefi_vm_boots_successfully() {
    let vm = test_uefi_vm("uefi")
        .build()
        .await
        .expect("Failed to build UEFI VM");

    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout(UEFI_BOOT_SUCCESS_MESSAGE, Duration::from_secs(60))
        .await
        .expect("UEFI VM did not boot successfully");

    vm.kill().await.expect("Failed to stop UEFI VM");
}

#[apple_main::harness_test]
async fn test_uefi_pool_vms_boot_independently() {
    let pool = test_uefi_pool("uefi")
        .build(2)
        .await
        .expect("Failed to build UEFI pool");

    // Reserve first VM
    let vm1 = pool.reserve().await.expect("Failed to reserve first VM");
    let console1 = vm1.console().await.expect("Failed to get console for VM 1");
    console1
        .wait_for_timeout(UEFI_BOOT_SUCCESS_MESSAGE, Duration::from_secs(60))
        .await
        .expect("First UEFI VM did not boot successfully");

    // Reserve second VM (should have its own EFI variable store)
    let vm2 = pool.reserve().await.expect("Failed to reserve second VM");
    let console2 = vm2.console().await.expect("Failed to get console for VM 2");
    console2
        .wait_for_timeout(UEFI_BOOT_SUCCESS_MESSAGE, Duration::from_secs(60))
        .await
        .expect("Second UEFI VM did not boot successfully");

    // Both VMs should be independent
    drop(vm1);
    drop(vm2);
}
