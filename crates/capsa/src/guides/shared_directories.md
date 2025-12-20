# Shared Directories

Capsa can share host directories with the guest VM, enabling file exchange
without disk images. This is particularly useful for:

- Mounting build artifacts for testing
- Sharing source code for development
- Collecting output from the guest

## Basic Sharing

Use the [`share`](crate::LinuxVmBuilder::share) method on the builder:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig, MountMode};

# async fn example() -> capsa::Result<()> {
let config = LinuxDirectBootConfig::new("./kernel", "./initrd");

let vm = Capsa::vm(config)
    .share("./src", "/mnt/src", MountMode::ReadOnly)
    .share("./output", "/mnt/output", MountMode::ReadWrite)
    .console_enabled()
    .build()
    .await?;
# Ok(())
# }
```

The guest sees these directories at the specified mount points.

## Mount Modes

[`MountMode`](crate::MountMode) controls access permissions:

- **`ReadOnly`** - Guest can read but not modify files. Use for source code,
  binaries, or any data that shouldn't change.

- **`ReadWrite`** - Guest can read and write. Use for output directories or
  when the guest needs to modify files.

```rust,no_run
use capsa::MountMode;

# fn example() {
// Safe: guest can run but not modify your binary
let mode = MountMode::ReadOnly;

// For output collection
let mode = MountMode::ReadWrite;
# }
```

## Integration Testing Pattern

A common pattern is sharing build artifacts read-only and collecting output:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig, MountMode};
use std::time::Duration;

# async fn example() -> capsa::Result<()> {
let config = LinuxDirectBootConfig::new("./kernel", "./initrd");

let vm = Capsa::vm(config)
    .share("./target/release", "/app", MountMode::ReadOnly)
    .share("./test-results", "/results", MountMode::ReadWrite)
    .console_enabled()
    .build()
    .await?;

let console = vm.console().await?;
console.wait_for_timeout("~ #", Duration::from_secs(30)).await?;

// Run tests and write results
console.write_line("/app/my-tests --output /results/report.xml").await?;
console.wait_for_timeout("Tests complete", Duration::from_secs(60)).await?;

vm.kill().await?;

// Results are now available at ./test-results/report.xml
# Ok(())
# }
```

## Share Mechanisms

Capsa supports two underlying mechanisms for directory sharing:

- **virtio-fs** - Higher performance, recommended when available
- **virtio-9p** - Wider compatibility, fallback option

By default, Capsa selects the best available mechanism. For explicit control,
use [`SharedDir`](crate::SharedDir) directly:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig, SharedDir, MountMode};

# async fn example() -> capsa::Result<()> {
let config = LinuxDirectBootConfig::new("./kernel", "./initrd");

let share = SharedDir::new("./data", "/mnt/data")
    .with_mode(MountMode::ReadOnly);

let vm = Capsa::vm(config)
    .shared_dir(share)
    .build()
    .await?;
# Ok(())
# }
```

## Tips

- **Use read-only mounts** for anything the guest shouldn't modify
- **Create output directories** on the host before starting the VM
- **Avoid sharing large directory trees** as this can impact performance
- **Consider disk images** for large datasets that don't change frequently
