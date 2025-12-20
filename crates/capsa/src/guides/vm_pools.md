# VM Pools

A [`VmPool`](crate::VmPool) pre-creates identical VMs that can be reserved for
temporary use. This amortizes VM startup costs when running many short-lived
workloads.

## Creating a Pool

Use [`Capsa::pool`](crate::Capsa::pool) to create a pool builder, then call
`.build(size)` with the number of VMs:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig};

# async fn example() -> capsa::Result<()> {
let config = LinuxDirectBootConfig::new("./kernel", "./initrd");

let pool = Capsa::pool(config)
    .cpus(2)
    .memory_mb(512)
    .console_enabled()
    .build(5)  // Create 5 identical VMs
    .await?;
# Ok(())
# }
```

All VMs in the pool share the same configuration.

## Reserving VMs

Call [`reserve`](crate::VmPool::reserve) to get a VM from the pool:

```rust,no_run
# async fn example(pool: capsa::VmPool) -> capsa::Result<()> {
let vm = pool.reserve().await?;

// Use VmHandle methods directly (PooledVm implements Deref<Target=VmHandle>)
let console = vm.console().await?;
console.wait_for("login:").await?;
console.write_line("whoami").await?;

// VM is killed and replaced when `vm` goes out of scope
# Ok(())
# }
```

When the [`PooledVm`](crate::PooledVm) is dropped, it is automatically killed
and a fresh VM is spawned to maintain the pool size.

## Non-Blocking Reservation

Use [`try_reserve`](crate::VmPool::try_reserve) when you can't afford to wait:

```rust,no_run
# fn example(pool: capsa::VmPool) -> capsa::Result<()> {
match pool.try_reserve() {
    Ok(vm) => { /* use vm */ }
    Err(capsa::Error::PoolEmpty) => { /* no VMs available, handle gracefully */ }
    Err(e) => return Err(e),
}
# Ok(())
# }
```

## Integration Testing Pattern

Pools are ideal for parallel test execution:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig, MountMode};
use std::sync::Arc;
use std::time::Duration;

# async fn example() -> capsa::Result<()> {
let config = LinuxDirectBootConfig::new("./kernel", "./initrd");

let pool = Arc::new(
    Capsa::pool(config)
        .share("./target/release", "/app", MountMode::ReadOnly)
        .console_enabled()
        .build(4)
        .await?
);

// Run tests in parallel
let mut handles = vec![];
for test_name in ["test_a", "test_b", "test_c", "test_d"] {
    let pool = Arc::clone(&pool);
    handles.push(tokio::spawn(async move {
        let vm = pool.reserve().await?;
        let console = vm.console().await?;

        console.wait_for_timeout("~ #", Duration::from_secs(30)).await?;
        console.write_line(&format!("/app/run-test {}", test_name)).await?;
        console.wait_for_timeout("PASSED", Duration::from_secs(60)).await?;

        Ok::<_, capsa::Error>(())
    }));
}

for handle in handles {
    handle.await.unwrap()?;
}
# Ok(())
# }
```

## Limitations

- **No additional disks**: Pool VMs cannot use `.disk()`. Root disks set via
  [`LinuxDirectBootConfig::with_root_disk`](crate::LinuxDirectBootConfig::with_root_disk)
  are allowed but should be read-only to avoid state leaking between reservations.

- **Silent respawn failures**: If spawning a replacement VM fails, the pool
  size decreases silently (logged at error level).

## When to Use Pools

Pools are useful when:
- Running many short-lived workloads
- You need fresh VM state for each task
- VM startup time is significant compared to workload duration
- Running parallel tests that each need isolated VMs

For long-running VMs or VMs with unique configurations, use
[`Capsa::vm`](crate::Capsa::vm) instead.
