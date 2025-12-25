# Console Automation Flakiness Investigation

This document captures research and findings about flaky network tests in capsa, specifically related to serial console automation timing issues.

## Problem Statement

Network tests that run multiple commands in sequence through the VM console are flaky when run in parallel. Tests pass 100% when run in isolation but fail 60-80% of the time with 4 concurrent test threads.

### Symptoms

- Tests timeout waiting for expected output (e.g., `HTTP_BLOCKED`)
- Failures occur at exactly 30 seconds (the configured timeout)
- The same test passes immediately when re-run in isolation
- Reducing `--test-threads` to 2-3 improves reliability

### Affected Tests

Primarily policy tests that execute multiple commands in sequence:
- `test_policy_deny_specific_port`
- `test_policy_allow_https_only`
- `test_policy_allow_specific_ip`
- `test_policy_deny_all_allow_dns`

## Root Cause Analysis

### Initial Hypotheses (Ruled Out)

1. **macOS Virtualization framework limits** - Ruled out because the issue persists even with 2 threads
2. **Socket buffer exhaustion** - Increasing buffer to 256KB didn't help
3. **Port conflicts** - Fixed port conflicts but flakiness remained
4. **TCP RST not being sent** - Added RST for denied connections but issue persisted

### Actual Root Cause: Serial Console Automation Timing

The issue is a well-documented problem in serial console automation, not specific to busybox or our implementation.

#### Key Insight from Industry Research

From [5 Serial Automation Gotchas](https://www.thegoodpenguin.co.uk/blog/5-serial-automation-gotchas/):

> "If you transmit characters too quickly, especially on serial interfaces without flow-control, not all of those characters will arrive. Buffers from the hardware UART right through to the application may overflow."

#### The "Fence Post" Pattern

The recommended solution is to use shell prompts as "fence posts" between commands:

> "Using command prompts as 'fence posts' ensures reliable sequencing."
> "A sleep in a test should be seen as a symptom for a missing expect statement."

#### PTY Buffer Behavior

From [Cloud Hypervisor Issue #3004](https://github.com/cloud-hypervisor/cloud-hypervisor/issues/3004):

> "When the buffer for serial port is full, the guest is stalled."

The PTY buffer between the VM and host can cause timing issues:
1. Commands sent too quickly may overflow buffers
2. Output may not be flushed before the next command arrives
3. The shell may not be ready to accept input

## Current Workaround

Adding a 100ms delay between sequential commands fixes the issue:

```rust
console.write_line("command1 && echo SUCCESS").await?;
console.wait_for_timeout("SUCCESS", Duration::from_secs(30)).await?;

// Delay allows buffers to flush and shell to be ready
tokio::time::sleep(Duration::from_millis(100)).await;

console.write_line("command2 && echo DONE").await?;
console.wait_for_timeout("DONE", Duration::from_secs(30)).await?;
```

This works but is not ideal because:
- Arbitrary delays are fragile and may need tuning
- Slows down tests unnecessarily
- Doesn't address the underlying issue

## Proposed Proper Fixes

### Option 1: Wait for Shell Prompt

Wait for the shell prompt (`# ` or `$ `) after each command:

```rust
console.write_line("command && echo SUCCESS").await?;
console.wait_for_timeout("SUCCESS", Duration::from_secs(30)).await?;
console.wait_for_timeout("# ", Duration::from_secs(5)).await?; // Wait for prompt
```

**Challenges:**
- Prompt may appear in command output (false positives)
- Prompt format may vary between shells
- Kernel messages can interleave with prompt

### Option 2: Unique Completion Markers

Append a unique marker to each command and wait for it:

```rust
let marker = format!("__DONE_{}__", uuid::Uuid::new_v4());
console.write_line(&format!("command; echo {}", marker)).await?;
console.wait_for_timeout(&marker, Duration::from_secs(30)).await?;
```

**Challenges:**
- Marker could theoretically appear in command output
- Still subject to buffer timing issues
- We tried this (`exec` method) and it still failed under load

### Option 3: Async-Aware Buffer Flushing

Ensure the console reader has fully processed all pending output before sending the next command:

```rust
// After waiting for pattern, drain any remaining buffered data
console.flush_read_buffer().await?;
// Small yield to let async I/O complete
tokio::task::yield_now().await;
```

### Option 4: Rate-Limited Command Sending

Implement inter-character delays when sending commands (like Expect's `send -h`):

```rust
impl VmConsole {
    async fn write_line_slow(&self, s: &str, char_delay: Duration) -> Result<()> {
        for c in s.chars() {
            self.write(&[c as u8]).await?;
            tokio::time::sleep(char_delay).await;
        }
        self.write(b"\n").await?;
        Ok(())
    }
}
```

### Option 5: Proper PTY Flow Control

Investigate if we can enable flow control on the socketpair/PTY:
- XON/XOFF software flow control
- Larger buffer sizes on both ends
- Synchronous flush before next command

## Investigation Tasks

### High Priority

1. [ ] Determine why `exec` method with `__CMD_COMPLETE__` marker still fails
   - Add debug logging to see if marker is received
   - Check if marker gets interleaved with next command's output

2. [ ] Test the prompt-waiting approach more thoroughly
   - Use regex to match prompt at start of line: `^\s*#\s*$`
   - Handle kernel message interleaving

3. [ ] Investigate PTY buffer sizes and flow control
   - Check default buffer sizes on macOS
   - Test with SO_RCVBUF increase on both ends
   - Research if flow control is possible

### Medium Priority

4. [ ] Profile the async I/O path
   - Add timing logs to console read/write
   - Check if tokio polling is fast enough
   - Test with different polling intervals

5. [ ] Compare with other VM automation tools
   - How does libvirt handle console automation?
   - What does QEMU's `-serial stdio` do differently?

6. [ ] Test on Linux (KVM backend)
   - Does the issue reproduce with virtio-console?
   - Is it specific to macOS socketpair implementation?

### Low Priority

7. [ ] Consider alternative architectures
   - SSH into VM instead of serial console
   - virtio-vsock for host-guest communication
   - Shared filesystem for command/response

## References

- [5 Serial Automation Gotchas](https://www.thegoodpenguin.co.uk/blog/5-serial-automation-gotchas/)
- [Cloud Hypervisor Serial Buffering Issue](https://github.com/cloud-hypervisor/cloud-hypervisor/issues/3004)
- [Red Hat PTY Buffer Bug](https://bugzilla.redhat.com/show_bug.cgi?id=1455451)
- [Expect Wikipedia](https://en.wikipedia.org/wiki/Expect)
- [BusyBox Ash Race Conditions](https://lists.busybox.net/pipermail/busybox/2009-May/069314.html)

## Test Commands

```bash
# Run flaky test in isolation (should pass)
cargo test --features macos-subprocess -p capsa --test network_test test_policy_deny_specific_port

# Run all network tests with 4 threads (may fail)
cargo test --features macos-subprocess -p capsa --test network_test -- --test-threads=4

# Run with reduced parallelism (more reliable)
cargo test --features macos-subprocess -p capsa --test network_test -- --test-threads=2

# Run flakiness test script
for i in $(seq 1 10); do
  cargo test --features macos-subprocess -p capsa --test network_test -- --test-threads=4 2>&1 | grep "test result"
done
```

## Appendix: Console Implementation

The current console implementation in `crates/capsa/src/console.rs`:

- Uses `tokio::io::BufReader` for buffered reading
- `wait_for()` accumulates lines until pattern is found
- Pattern matching drains buffer up to match point
- No explicit flow control or synchronization

Key code paths to investigate:
- `VmConsole::wait_for()` - pattern matching and buffer management
- `SocketPairDevice::send()` - frame transmission
- `SocketPairDevice::poll_recv()` - frame reception
