# Known Issues

This document tracks known issues and quirks that need further investigation.

## 1. VmConsole::exec() Hangs with Shell Pipes

**Status**: Workaround available, root cause not fully understood
**Discovered**: 2025-12-25
**Affects**: `VmConsole::exec()` in `crates/capsa/src/console.rs`

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
- The KVM-specific tests using `wait_for_and_send()` (which waits for a simple
  pattern without appending a marker) work with pipes

### Theories to Investigate

1. **Shell parsing interaction**: The `;` after a pipeline might be parsed
   differently by busybox ash, causing the marker echo to not execute or
   its output to be lost.

2. **Pipeline buffering**: When a pipe is involved, stdout buffering behavior
   may change. The marker might be buffered and not flushed to the console.

3. **PTY/console interaction**: The pipeline might interact with the PTY in a
   way that prevents the marker from being written to the console output that
   we're reading.

4. **read_line() blocking**: The `wait_for()` method uses `BufReader::read_line()`.
   If the pipeline changes how newlines are output, `read_line()` might block
   waiting for a newline that never comes.

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
3. Test if the issue occurs on macOS or only on KVM
4. Examine the raw bytes being sent/received to check for buffering issues
5. Try adding explicit `sync` or newline after the marker
