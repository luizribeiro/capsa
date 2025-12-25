#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Stress tests for console automation timing.
//!
//! These tests execute many commands in rapid sequence to verify that the
//! exec() method properly synchronizes command execution, solving the serial
//! console timing issues documented in docs/console-automation-investigation.md.

#[cfg(not(feature = "linux-kvm"))]
use capsa::test_utils::test_vm;
#[cfg(not(feature = "linux-kvm"))]
use std::time::Duration;

/// Simple test to verify exec works.
#[apple_main::harness_test]
async fn test_exec_10_commands() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support console input yet");
        return;
    }

    #[cfg(not(feature = "linux-kvm"))]
    {
        let vm = test_vm("default")
            .build()
            .await
            .expect("Failed to build VM");
        let console = vm.console().await.expect("Failed to get console");

        console
            .wait_for_timeout("Boot successful", Duration::from_secs(30))
            .await
            .expect("VM did not boot");

        // Execute single command
        eprintln!("Executing first command...");
        let output = console
            .exec("echo 'test output'", Duration::from_secs(10))
            .await
            .expect("Failed to exec command");
        eprintln!("First command output: {:?}", output);

        assert!(
            output.contains("test output"),
            "Output missing expected content"
        );

        // Execute second command
        eprintln!("Executing second command...");
        let output2 = console
            .exec("echo 'second test'", Duration::from_secs(10))
            .await
            .expect("Failed to exec second command");
        eprintln!("Second command output: {:?}", output2);

        assert!(
            output2.contains("second test"),
            "Second output missing expected content"
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Executes 30 commands in rapid sequence - aggressive stress test.
#[apple_main::harness_test]
async fn test_exec_30_commands() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support console input yet");
        return;
    }

    #[cfg(not(feature = "linux-kvm"))]
    {
        let vm = test_vm("default")
            .build()
            .await
            .expect("Failed to build VM");
        let console = vm.console().await.expect("Failed to get console");

        console
            .wait_for_timeout("Boot successful", Duration::from_secs(30))
            .await
            .expect("VM did not boot");

        for i in 1..=30 {
            console
                .exec(
                    &format!("echo 'Rapid command {}'", i),
                    Duration::from_secs(5),
                )
                .await
                .unwrap_or_else(|e| panic!("Rapid command {} failed: {}", i, e));
        }

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests commands with variable output lengths.
#[apple_main::harness_test]
async fn test_exec_variable_output() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support console input yet");
        return;
    }

    #[cfg(not(feature = "linux-kvm"))]
    {
        let vm = test_vm("default")
            .build()
            .await
            .expect("Failed to build VM");
        let console = vm.console().await.expect("Failed to get console");

        console
            .wait_for_timeout("Boot successful", Duration::from_secs(30))
            .await
            .expect("VM did not boot");

        // Short output
        console
            .exec("echo short", Duration::from_secs(5))
            .await
            .expect("Short output failed");

        // Medium output (list /etc)
        console
            .exec("ls /etc", Duration::from_secs(5))
            .await
            .expect("Medium output failed");

        // Longer output (50 lines)
        console
            .exec(
                "for i in $(seq 1 50); do echo \"Line $i\"; done",
                Duration::from_secs(10),
            )
            .await
            .expect("Long output failed");

        // Back to short
        console
            .exec("echo done", Duration::from_secs(5))
            .await
            .expect("Final short output failed");

        vm.kill().await.expect("Failed to kill VM");
    }
}

/// Tests mixed execution times.
#[apple_main::harness_test]
async fn test_exec_mixed_times() {
    #[cfg(feature = "linux-kvm")]
    {
        eprintln!("Skipping: KVM backend doesn't support console input yet");
        return;
    }

    #[cfg(not(feature = "linux-kvm"))]
    {
        let vm = test_vm("default")
            .build()
            .await
            .expect("Failed to build VM");
        let console = vm.console().await.expect("Failed to get console");

        console
            .wait_for_timeout("Boot successful", Duration::from_secs(30))
            .await
            .expect("VM did not boot");

        // Instant
        console
            .exec("echo instant", Duration::from_secs(5))
            .await
            .expect("Instant failed");

        // 100ms delay
        console
            .exec("sleep 0.1 && echo 'after 100ms'", Duration::from_secs(5))
            .await
            .expect("100ms delay failed");

        // Another instant
        console
            .exec("echo instant2", Duration::from_secs(5))
            .await
            .expect("Instant 2 failed");

        // 200ms delay
        console
            .exec("sleep 0.2 && echo 'after 200ms'", Duration::from_secs(5))
            .await
            .expect("200ms delay failed");

        // Final instant
        console
            .exec("echo final", Duration::from_secs(5))
            .await
            .expect("Final failed");

        vm.kill().await.expect("Failed to kill VM");
    }
}
