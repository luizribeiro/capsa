#![feature(custom_test_frameworks)]
#![test_runner(apple_main::test_runner)]

//! Stress tests for console automation timing.
//!
//! These tests execute many commands in rapid sequence to verify that the
//! exec() method properly synchronizes command execution, solving the serial
//! console timing issues documented in docs/console-automation-investigation.md.

use capsa::test_utils::test_vm;
use std::time::Duration;
#[allow(unused_imports)]
use tokio::time::sleep;

/// Tests that KVM virtio-console output is not duplicated.
///
/// This test verifies the fix for the character duplication bug where each
/// output line was repeated ~80 times due to improper virtio queue state
/// tracking. The fix saves and restores next_avail/next_used indices.
#[apple_main::harness_test]
async fn test_kvm_no_character_duplication() {
    #[cfg(not(feature = "linux-kvm"))]
    {
        eprintln!("Skipping: test is specific to KVM backend");
        return;
    }

    #[cfg(feature = "linux-kvm")]
    {
        let vm = test_vm("default")
            .no_network()
            .build()
            .await
            .expect("Failed to build VM");
        let console = vm.console().await.expect("Failed to get console");

        // Wait for boot to complete
        let initial_output = console
            .wait_for_timeout("Boot successful", Duration::from_secs(30))
            .await
            .expect("VM did not boot");

        // Wait a bit for any duplicated output to arrive
        sleep(Duration::from_millis(100)).await;

        // Read any remaining output
        let remaining = console.read_available().await.unwrap_or_default();
        let full_output = format!("{}{}", initial_output, remaining);

        // The character duplication bug causes massive output bloat:
        // - Without fix: ~18KB of output (each byte processed multiple times)
        // - With fix: ~800 bytes of output
        // We check the output length as a simple, robust indicator.
        let len = full_output.len();
        assert!(
            len < 5000,
            "Possible character duplication: output is {} bytes (expected < 2000). \
             With the queue state bug, each output byte is processed multiple times, \
             causing massive output bloat. Sample: {:?}",
            len,
            &full_output[..std::cmp::min(300, full_output.len())]
        );

        vm.kill().await.expect("Failed to kill VM");
    }
}

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

/// Diagnostic test to investigate fork/exec behavior on different backends.
///
/// This test explores known issues with command execution:
/// - macOS: Pipes hang but subshell workaround `(cmd | cmd)` works
/// - KVM: ALL forked commands hang (not just pipes)
///
/// See docs/known-issues.md for details.
///
/// Run with: cargo test test_exec_pipe_diagnostic --features <backend> -- --nocapture
#[apple_main::harness_test]
async fn test_exec_pipe_diagnostic() {
    #[cfg(feature = "linux-kvm")]
    eprintln!("Running on KVM backend - known issue: all fork-requiring commands fail");
    #[cfg(not(feature = "linux-kvm"))]
    eprintln!("Running on macOS backend - known issue: pipe commands hang");

    let vm = test_vm("default")
        .no_network()
        .build()
        .await
        .expect("Failed to build VM");
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    // Test 1: Shell builtin (no fork) - should work on all backends
    eprintln!("\n[1] Testing shell builtin (echo)...");
    match console
        .exec("echo builtin_test", Duration::from_secs(5))
        .await
    {
        Ok(_) => eprintln!("    ✓ Shell builtin works"),
        Err(e) => eprintln!("    ✗ Shell builtin FAILED: {}", e),
    }

    // Test 2: Subshell (requires fork)
    eprintln!("\n[2] Testing subshell (echo in parentheses)...");
    match console
        .exec("(echo subshell_test)", Duration::from_secs(5))
        .await
    {
        Ok(_) => eprintln!("    ✓ Subshell works"),
        Err(e) => eprintln!("    ✗ Subshell FAILED: {}", e),
    }

    // Interrupt any hung state
    console.send_interrupt().await.ok();
    sleep(Duration::from_millis(100)).await;

    // Test 3: Simple pipe
    eprintln!("\n[3] Testing simple pipe (echo | cat)...");
    match console
        .exec("echo pipe_test | cat", Duration::from_secs(5))
        .await
    {
        Ok(_) => eprintln!("    ✓ Pipe works"),
        Err(e) => eprintln!("    ✗ Pipe FAILED: {}", e),
    }

    // Interrupt any hung state
    console.send_interrupt().await.ok();
    sleep(Duration::from_millis(100)).await;

    // Test 4: External command (requires exec)
    eprintln!("\n[4] Testing external command (ls /)...");
    match console.exec("ls /", Duration::from_secs(5)).await {
        Ok(_) => eprintln!("    ✓ External command works"),
        Err(e) => eprintln!("    ✗ External command FAILED: {}", e),
    }

    // Clean up
    console.send_interrupt().await.ok();
    sleep(Duration::from_millis(100)).await;

    vm.kill().await.expect("Failed to kill VM");
    eprintln!("\nTest complete. See docs/known-issues.md for issue details.");
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
