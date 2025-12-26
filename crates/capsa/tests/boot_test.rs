//! Integration tests for VM boot functionality.

use capsa::test_utils::{default_test_vm, test_vm};
use std::time::Duration;

#[tokio::test]
async fn test_vm_boots_successfully() {
    let vm = default_test_vm().build().await.expect("Failed to build VM");
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot successfully");

    vm.kill().await.expect("Failed to stop VM");
}

#[tokio::test]
async fn test_sequential_vms() {
    for i in 0..2 {
        let vm = test_vm("default")
            .build()
            .await
            .unwrap_or_else(|e| panic!("Failed to build VM {}: {}", i, e));

        let console = vm.console().await.expect("Failed to get console");

        console
            .wait_for_timeout("Boot successful", Duration::from_secs(30))
            .await
            .unwrap_or_else(|e| panic!("VM {} did not boot: {}", i, e));

        vm.kill()
            .await
            .unwrap_or_else(|e| panic!("Failed to stop VM {}: {}", i, e));
    }
}
