#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Integration tests for VM console functionality.

#[cfg(not(feature = "linux-kvm"))]
use capsa::test_utils::test_vm;
#[cfg(not(feature = "linux-kvm"))]
use std::time::Duration;

#[apple_main::harness_test]
async fn test_console_echo() {
    // TODO: KVM backend doesn't support interactive console input yet (needs IRQ injection)
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping test_console_echo on KVM backend (console input not yet supported)");
        return;
    }

    #[cfg(not(feature = "linux-kvm"))]
    {
        let vm = test_vm("no-network")
            .build()
            .await
            .expect("Failed to build VM");
        let console = vm.console().await.expect("Failed to get console");

        console
            .wait_for_timeout("Boot successful", Duration::from_secs(30))
            .await
            .expect("VM did not boot");

        tokio::time::sleep(Duration::from_millis(50)).await;

        console
            .write_line("echo hello-from-test")
            .await
            .expect("Failed to write");

        let output = console
            .wait_for_timeout("hello-from-test", Duration::from_secs(5))
            .await
            .expect("Echo output not found");

        assert!(output.contains("hello-from-test"));

        vm.kill().await.expect("Failed to kill VM");
    }
}

#[apple_main::harness_test]
async fn test_console_ctrl_c() {
    // TODO: KVM backend doesn't support interactive console input yet (needs IRQ injection)
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping test_console_ctrl_c on KVM backend (console input not yet supported)");
        return;
    }

    #[cfg(not(feature = "linux-kvm"))]
    {
        let vm = test_vm("no-network")
            .build()
            .await
            .expect("Failed to build VM");
        let console = vm.console().await.expect("Failed to get console");

        console
            .wait_for_timeout("Boot successful", Duration::from_secs(30))
            .await
            .expect("VM did not boot");

        tokio::time::sleep(Duration::from_millis(50)).await;

        console
            .write_line("sleep 100")
            .await
            .expect("Failed to write");

        tokio::time::sleep(Duration::from_millis(50)).await;

        console
            .send_interrupt()
            .await
            .expect("Failed to send Ctrl+C");

        tokio::time::sleep(Duration::from_millis(50)).await;
        let output = console.read_available().await.expect("Failed to read");
        assert!(output.contains("^C"), "Ctrl+C was not received by VM");

        vm.kill().await.expect("Failed to kill VM");
    }
}
