# Getting Started with Capsa

This guide walks you through creating and running your first virtual machine
with Capsa.

## Prerequisites

Before using Capsa, you'll need:

1. **A Linux kernel image** (`bzImage` or similar) - The kernel to boot
2. **An initrd/initramfs** - Initial RAM disk with your userspace

For testing, you can build minimal images with tools like:
- [NixOS generators](https://github.com/nix-community/nixos-generators)
- [Buildroot](https://buildroot.org/)
- [mkosi](https://github.com/systemd/mkosi)

## Creating Your First VM

All VM creation starts with [`Capsa`](crate::Capsa). Use [`Capsa::vm`](crate::Capsa::vm)
to create a single VM:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig};

#[tokio::main]
async fn main() -> capsa::Result<()> {
    // Configure boot settings
    let config = LinuxDirectBootConfig::new("./bzImage", "./initrd");

    // Build and start the VM
    let vm = Capsa::vm(config)
        .cpus(2)
        .memory_mb(1024)
        .console_enabled()
        .build()
        .await?;

    // The VM is now running!
    println!("VM started");

    // Wait for it to exit
    let exit_code = vm.wait().await?;
    println!("VM exited with code: {}", exit_code);

    Ok(())
}
```

## Adding a Root Filesystem

Most VMs need a root filesystem. Add one via the boot configuration:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig, DiskImage};

# async fn example() -> capsa::Result<()> {
let config = LinuxDirectBootConfig::new("./bzImage", "./initrd")
    .with_root_disk(DiskImage::new("./rootfs.raw"));

let vm = Capsa::vm(config)
    .console_enabled()
    .build()
    .await?;
# Ok(())
# }
```

## Interacting via Console

Enable the console to interact with your VM programmatically:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig};
use std::time::Duration;

# async fn example() -> capsa::Result<()> {
let config = LinuxDirectBootConfig::new("./bzImage", "./initrd");

let vm = Capsa::vm(config)
    .console_enabled()  // Required!
    .build()
    .await?;

let console = vm.console().await?;

// Wait for boot to complete
console.wait_for_timeout("login:", Duration::from_secs(30)).await?;

// Log in
console.write_line("root").await?;

// Run a command
console.wait_for("#").await?;
console.write_line("uname -a").await?;
# Ok(())
# }
```

## Shutting Down

You have two options for stopping a VM:

```rust,no_run
# async fn example(vm: capsa::VmHandle) -> capsa::Result<()> {
// Graceful shutdown (sends ACPI power button event)
vm.stop().await?;

// Or: Immediate termination
vm.kill().await?;
# Ok(())
# }
```

## Next Steps

- [Console Automation](crate::guides::console_automation) - Testing patterns with the console
- [VM Pools](crate::guides::vm_pools) - Pre-warmed VMs for faster startup
- [Shared Directories](crate::guides::shared_directories) - File sharing between host and guest
