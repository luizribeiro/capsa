# Known Issues

This document tracks known issues and quirks that need further investigation.

## 1. VmConsole::exec() Hangs with Shell Pipes (macOS)

**Status**: Workaround available, root cause not fully understood
**Discovered**: 2025-12-25
**Affects**: `VmConsole::exec()` in `crates/capsa/src/console.rs`
**Backend**: macOS (Virtualization.framework)

### Description

When using `exec()` with commands containing shell pipes (`|`), the command hangs
indefinitely until timeout. Commands without pipes work correctly.

### Reproduction

```rust
// This hangs:
console.exec("echo hello | cat", Duration::from_secs(5)).await  // Timeout

// This works:
console.exec("echo hello", Duration::from_secs(5)).await  // OK
```

### How exec() Works

The `exec()` method appends `; echo __DONE_X__` to the command and waits for
`\n__DONE_X__` in the console output:

```rust
let full_cmd = format!("{} ; echo {}", cmd, marker);
self.write_line(&full_cmd).await?;
self.wait_for_timeout(&format!("\n{}", marker), timeout).await
```

### Observations

- Commands without pipes work fine (echo, wget to /dev/null, ping, nslookup)
- Any command with a pipe causes the hang, even simple ones like `echo | cat`
- The marker `__DONE_X__` should still be echoed since it uses `;` not `&&`
- Wrapping the pipe in a subshell fixes the issue: `(cmd1 | cmd2)`

### Workarounds

1. **Avoid pipes**: Restructure commands to not use pipes when possible
   ```rust
   // Instead of: wget ... | grep ...
   // Use: wget ... -O /dev/null && echo SUCCESS
   ```

2. **Use subshells**: Wrap piped commands in parentheses
   ```rust
   // Instead of: cmd1 | cmd2 && echo DONE
   // Use: (cmd1 | cmd2) && echo DONE
   ```

3. **Use wait_for() directly**: For complex commands, send the command with
   `write_line()` and use `wait_for_timeout()` with your own pattern

### Next Steps

1. Add debug logging to trace exactly what output is received
2. Test with different shells (ash, bash, sh) to isolate if it's busybox-specific
3. Examine the raw bytes being sent/received to check for buffering issues

---

## 2. KVM Console: Fork/Exec Fails (All Non-Builtin Commands)

**Status**: Under investigation
**Discovered**: 2025-12-25
**Affects**: KVM backend console I/O
**Backend**: Linux (KVM)

### Description

On the KVM backend, any command that requires forking a child process fails
to produce output. This includes:
- External commands (e.g., `cat`, `ls`)
- Subshells (e.g., `(echo hello)`)
- Pipelines (e.g., `echo hello | cat`)
- Redirections to external commands

Only shell builtins running in the main shell process work correctly.

### Reproduction

```rust
// These work (shell builtins, no fork):
console.exec("echo hello", Duration::from_secs(5)).await      // OK
console.exec("pwd", Duration::from_secs(5)).await             // OK

// These ALL fail (require fork):
console.exec("cat /etc/hosts", Duration::from_secs(5)).await  // Timeout
console.exec("(echo hello)", Duration::from_secs(5)).await    // Timeout
console.exec("echo hi | cat", Duration::from_secs(5)).await   // Timeout
console.exec("ls", Duration::from_secs(5)).await              // Timeout
```

### Observations

1. **Forking is the issue**: The common factor is that all failing commands
   require the shell to fork() a child process.

2. **No output at all**: Failing commands produce zero output - not even
   the command echo or error messages.

3. **Shell becomes unresponsive**: After a fork-requiring command, the shell
   stops responding entirely.

4. **Separate character duplication bug**: Console output shows massive
   character duplication (each char repeated ~80 times), suggesting a
   virtio-console queue handling issue.

### Root Cause Theories

1. **Console FD inheritance**: Child processes may not properly inherit
   the virtio-console file descriptors, causing their stdout to go nowhere.

2. **Virtio-console queue corruption**: The queue handling in
   `virtio_console.rs` recreates the Queue object on each operation,
   potentially losing state.

3. **IRQ delivery issues**: The irqfd mechanism for virtio-console
   interrupts may not be working correctly, preventing the guest from
   receiving completion notifications.

4. **Input duplication**: Console input is sent to BOTH virtio-console
   AND serial (`vm.rs:487-494`), which may cause issues if the guest
   reads from both.

### Character Duplication Sub-Issue

The console output shows each character/line repeated ~80 times:
```
sh: sh: sh: sh: ... (77 times)
can't access tty; job control turned off (repeated)
~ # ~ # ~ # ~ # ... (80 times)
```

This is likely caused by:
- Virtio queue being processed multiple times
- Or multiple interrupt deliveries for the same data

### Next Steps

1. **Fix input duplication**: Remove the duplicate input to serial device
   when virtio-console is the primary console

2. **Debug virtio-console queue handling**: Add logging to track queue
   indices and ensure proper used ring updates

3. **Check FD inheritance**: Verify that child processes can write to
   the console by testing with explicit `/dev/hvc0` writes

4. **Review irqfd setup**: Ensure interrupt delivery is working correctly
