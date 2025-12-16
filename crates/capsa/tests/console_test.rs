//! Integration tests for VM console functionality.

use capsa::test_utils::default_test_vm;
use std::time::Duration;

#[tokio::test]
async fn test_console_echo() {
    let vm = default_test_vm().build().await.expect("Failed to build VM");
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("ping", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    tokio::time::sleep(Duration::from_millis(500)).await;

    console
        .write_line("echo hello-from-test")
        .await
        .expect("Failed to write");

    let output = console
        .wait_for_timeout("hello-from-test", Duration::from_secs(5))
        .await
        .expect("Echo output not found");

    assert!(output.contains("hello-from-test"));

    vm.stop().await.expect("Failed to stop VM");
}

#[tokio::test]
async fn test_console_ctrl_c() {
    let vm = default_test_vm().build().await.expect("Failed to build VM");
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("ping", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    tokio::time::sleep(Duration::from_millis(500)).await;

    console
        .write_line("sleep 100")
        .await
        .expect("Failed to write");

    tokio::time::sleep(Duration::from_millis(500)).await;

    console
        .send_interrupt()
        .await
        .expect("Failed to send Ctrl+C");

    tokio::time::sleep(Duration::from_millis(500)).await;
    let output = console.read_available().await.expect("Failed to read");
    assert!(output.contains("^C"), "Ctrl+C was not received by VM");

    vm.stop().await.expect("Failed to stop VM");
}
