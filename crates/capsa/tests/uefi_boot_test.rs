#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for UEFI boot functionality.

use capsa::test_utils::test_uefi_vm;
use std::time::Duration;

const BOOT_SUCCESS_MESSAGE: &str = "UEFI Boot";

#[apple_main::harness_test]
async fn test_uefi_vm_boots_successfully() {
    let vm = test_uefi_vm("uefi")
        .build()
        .await
        .expect("Failed to build UEFI VM");

    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout(BOOT_SUCCESS_MESSAGE, Duration::from_secs(30))
        .await
        .expect("UEFI VM did not boot successfully");

    vm.kill().await.expect("Failed to stop UEFI VM");
}
