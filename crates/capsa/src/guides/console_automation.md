# Console Automation

The [`VmConsole`](crate::VmConsole) enables programmatic interaction with a VM's
serial console. This is particularly useful for integration testing, where you
need to verify behavior inside the VM.

## Enabling the Console

The console must be explicitly enabled when building the VM:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig};

# async fn example() -> capsa::Result<()> {
let config = LinuxDirectBootConfig::new("./kernel", "./initrd");

let vm = Capsa::vm(config)
    .console_enabled()  // Required!
    .build()
    .await?;

let console = vm.console().await?;
# Ok(())
# }
```

## Basic Operations

### Waiting for Output

Use `wait_for` to block until specific text appears:

```rust,no_run
# async fn example(console: capsa::VmConsole) -> capsa::Result<()> {
// Wait indefinitely for login prompt
console.wait_for("login:").await?;

// Wait with timeout (recommended)
use std::time::Duration;
console.wait_for_timeout("login:", Duration::from_secs(30)).await?;
# Ok(())
# }
```

### Sending Input

Use `write_line` to send text followed by a newline:

```rust,no_run
# async fn example(console: capsa::VmConsole) -> capsa::Result<()> {
console.write_line("root").await?;
console.write_line("echo hello").await?;
# Ok(())
# }
```

## Integration Testing Pattern

A typical integration test follows this pattern:

```rust,no_run
use capsa::{Capsa, LinuxDirectBootConfig, MountMode};
use std::time::Duration;

# async fn example() -> capsa::Result<()> {
let config = LinuxDirectBootConfig::new("./kernel", "./initrd");

let vm = Capsa::vm(config)
    .share("./target/release", "/app", MountMode::ReadOnly)
    .console_enabled()
    .build()
    .await?;

let console = vm.console().await?;

// Wait for boot
console.wait_for_timeout("~ #", Duration::from_secs(30)).await?;

// Run your binary
console.write_line("/app/my-binary --test").await?;

// Verify expected output
console.wait_for_timeout("TEST PASSED", Duration::from_secs(10)).await?;

vm.kill().await?;
# Ok(())
# }
```

## Reading Raw Output

For more control, use the reader directly:

```rust,ignore
use tokio::io::AsyncBufReadExt;

# async fn example(console: capsa::VmConsole) -> capsa::Result<()> {
let (reader, _writer) = console.into_split();
let mut lines = reader.lines();

while let Some(line) = lines.next_line().await? {
    println!("VM: {}", line);
    if line.contains("shutdown") {
        break;
    }
}
# Ok(())
# }
```

## Tips

- **Always use timeouts** in tests to avoid hanging on failures
- **Match specific prompts** rather than generic patterns to avoid false matches
- **Share build artifacts** read-only to test binaries built on the host
- **Print markers** from your test code that are easy to `wait_for`
