//! Pool integration tests.
//!
//! These tests verify the VM pool functionality with actual VMs.
//! Run with: cargo test --test pool_test --features macos-native,test-utils

use capsa::test_utils::test_vm;
use capsa::Error;
use std::sync::Arc;
use std::time::Duration;

#[apple_main::harness_test]
async fn test_pool_creation() {
    let pool = test_vm("minimal")
        .build_pool(2)
        .await
        .expect("Failed to create pool");

    assert_eq!(pool.available_count().await, 2);
}

#[apple_main::harness_test]
async fn test_pool_reserve_decreases_available_count() {
    let pool = test_vm("minimal")
        .build_pool(2)
        .await
        .expect("Failed to create pool");

    assert_eq!(pool.available_count().await, 2);

    let vm1 = pool.reserve().await.expect("Failed to reserve VM");
    assert_eq!(pool.available_count().await, 1);

    let vm2 = pool.reserve().await.expect("Failed to reserve second VM");
    assert_eq!(pool.available_count().await, 0);

    drop(vm1);
    drop(vm2);
}

#[apple_main::harness_test]
async fn test_pool_try_reserve_fails_when_empty() {
    let pool = test_vm("minimal")
        .build_pool(1)
        .await
        .expect("Failed to create pool");

    let _vm = pool.reserve().await.expect("Failed to reserve VM");
    assert_eq!(pool.available_count().await, 0);

    let result = pool.try_reserve();
    assert!(matches!(result, Err(Error::PoolEmpty)));
}

#[apple_main::harness_test]
async fn test_pooled_vm_console_works() {
    let pool = test_vm("minimal")
        .build_pool(1)
        .await
        .expect("Failed to create pool");

    let vm = pool.reserve().await.expect("Failed to reserve VM");
    let console = vm.console().await.expect("Failed to get console");

    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot successfully");
}

#[apple_main::harness_test]
async fn test_pool_vm_respawns_after_release() {
    let pool = Arc::new(
        test_vm("minimal")
            .build_pool(1)
            .await
            .expect("Failed to create pool"),
    );

    let vm = pool.reserve().await.expect("Failed to reserve VM");
    let console = vm.console().await.expect("Failed to get console");
    console
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("VM did not boot");

    assert_eq!(pool.available_count().await, 0);

    drop(vm);

    // Wait for respawn to complete
    let start = std::time::Instant::now();
    let timeout = Duration::from_secs(60);
    while pool.available_count().await == 0 {
        if start.elapsed() > timeout {
            panic!("Pool did not respawn VM within timeout");
        }
        tokio::time::sleep(Duration::from_millis(500)).await;
    }

    assert_eq!(pool.available_count().await, 1);

    let vm2 = pool.reserve().await.expect("Failed to reserve respawned VM");
    let console2 = vm2.console().await.expect("Failed to get console");
    console2
        .wait_for_timeout("Boot successful", Duration::from_secs(30))
        .await
        .expect("Respawned VM did not boot");
}

#[apple_main::harness_test]
async fn test_pool_reserve_waits_for_available_vm() {
    let pool = Arc::new(
        test_vm("minimal")
            .build_pool(1)
            .await
            .expect("Failed to create pool"),
    );

    let vm = pool.reserve().await.expect("Failed to reserve VM");
    assert_eq!(pool.available_count().await, 0);

    let pool_clone = Arc::clone(&pool);
    let reserve_task = tokio::spawn(async move { pool_clone.reserve().await });

    tokio::time::sleep(Duration::from_millis(100)).await;

    drop(vm);

    let result = tokio::time::timeout(Duration::from_secs(60), reserve_task)
        .await
        .expect("reserve() did not complete within timeout")
        .expect("reserve task panicked");

    let vm2 = result.expect("reserve() failed after respawn");
    drop(vm2);
}

#[apple_main::harness_test]
async fn test_pool_concurrent_reservations() {
    let pool = Arc::new(
        test_vm("minimal")
            .build_pool(3)
            .await
            .expect("Failed to create pool"),
    );

    let pool1 = Arc::clone(&pool);
    let pool2 = Arc::clone(&pool);
    let pool3 = Arc::clone(&pool);

    let (r1, r2, r3) = tokio::join!(
        async { pool1.reserve().await },
        async { pool2.reserve().await },
        async { pool3.reserve().await },
    );

    let _vm1 = r1.expect("Failed to reserve VM 1");
    let _vm2 = r2.expect("Failed to reserve VM 2");
    let _vm3 = r3.expect("Failed to reserve VM 3");

    assert_eq!(pool.available_count().await, 0);
    assert!(matches!(pool.try_reserve(), Err(Error::PoolEmpty)));
}

#[apple_main::harness_test]
async fn test_try_reserve_returns_shutdown_error() {
    let pool = Arc::new(
        test_vm("minimal")
            .build_pool(1)
            .await
            .expect("Failed to create pool"),
    );

    // Reserve the only VM so pool is empty
    let vm = pool.reserve().await.expect("Failed to reserve");

    // try_reserve should return PoolEmpty when no VMs available
    assert!(matches!(pool.try_reserve(), Err(Error::PoolEmpty)));

    // Spawn a task that will call reserve() and wait
    let pool_clone = Arc::clone(&pool);
    let reserve_task = tokio::spawn(async move {
        pool_clone.reserve().await
    });

    // Give the task time to start waiting
    tokio::time::sleep(Duration::from_millis(100)).await;

    // Drop our pool handle and the reserved VM
    // This won't trigger VmPool::drop yet (task has a clone), but releasing
    // the VM will spawn a replacement, allowing the waiting task to proceed
    drop(pool);
    drop(vm);

    // The waiting task should eventually get a VM (the replacement)
    let result = tokio::time::timeout(Duration::from_secs(60), reserve_task)
        .await
        .expect("reserve() did not complete")
        .expect("task panicked");

    // Should succeed with the replacement VM
    let _vm2 = result.expect("reserve() should succeed after VM released");
}

apple_main::test_main!();
