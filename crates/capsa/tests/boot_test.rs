//! Integration tests for VM boot functionality.

use capsa::test_utils::default_test_vm;
use std::time::Duration;

#[tokio::test]
async fn test_vm_boots_successfully() {
    let vm = default_test_vm().build().await.expect("Failed to build VM");
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot successfully");

    vm.stop().await.expect("Failed to stop VM");
}
