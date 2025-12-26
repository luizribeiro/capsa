# Known Issues

This document tracks known issues and quirks that need further investigation.

## 1. ~~VmConsole::exec() Hangs with Shell Pipes (macOS)~~ RESOLVED

**Status**: Resolved or intermittent - tests now pass
**Discovered**: 2025-12-25
**Resolved**: 2025-12-25
**Backend**: macOS (Virtualization.framework)

### Original Issue

Pipe commands were reported to hang when using `exec()`. However, diagnostic
testing shows all command types now work correctly on macOS:

| Test | Result |
|------|--------|
| Shell builtin (`echo`) | ✓ works |
| Subshell (`(echo hello)`) | ✓ works |
| Pipe (`echo \| cat`) | ✓ works |
| External command (`ls /`) | ✓ works |

### Possible Explanations

1. The issue may have been fixed by `exec()` implementation improvements
2. The issue may only manifest under specific conditions (parallel tests, timing)
3. The issue may be intermittent

### Test Command

```bash
cargo test-macos-subprocess -p capsa --test console_stress_test test_exec_pipe_diagnostic -- --nocapture
```

If the issue reoccurs, document the specific reproduction conditions.

---

## 2. ~~KVM Console: Character Duplication~~ RESOLVED

**Status**: Resolved
**Discovered**: 2025-12-25
**Resolved**: 2025-12-25
**Backend**: Linux (KVM)

### Original Issue

Console output showed each character/line repeated ~80 times:
```
sh: sh: sh: sh: ... (77 times)
can't access tty; job control turned off (repeated)
~ # ~ # ~ # ~ # ... (80 times)
```

### Root Cause

The virtio-console queue handling in `virtio_console.rs` was recreating the
Queue object on each operation without preserving the `next_avail` and
`next_used` indices. This caused the same descriptors to be processed
multiple times, resulting in duplicate output.

### Fix

Added `next_avail` and `next_used` fields to `VirtioQueueState` and
save/restore them after each queue operation (commit fbf8b8e).

### Test

```bash
cargo test-linux --test console_stress_test test_kvm_no_character_duplication -- --nocapture
```

---

## 3. KVM Console: Fork/Exec Fails (All Non-Builtin Commands)

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

### Root Cause Theories

1. **Console FD inheritance**: Child processes may not properly inherit
   the virtio-console file descriptors, causing their stdout to go nowhere.

2. **IRQ delivery issues**: The irqfd mechanism for virtio-console
   interrupts may not be working correctly, preventing the guest from
   receiving completion notifications.

### Next Steps

1. **Check FD inheritance**: Verify that child processes can write to
   the console by testing with explicit `/dev/hvc0` writes

2. **Review irqfd setup**: Ensure interrupt delivery is working correctly
