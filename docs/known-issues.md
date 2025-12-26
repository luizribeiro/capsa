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

## 3. ~~KVM Console: Fork/Exec Fails (All Non-Builtin Commands)~~ RESOLVED

**Status**: Resolved
**Discovered**: 2025-12-25
**Resolved**: 2025-12-25
**Backend**: Linux (KVM)

### Original Issue

On the KVM backend, any command that required forking a child process failed
to produce output. This included external commands, subshells, and pipelines.
Only shell builtins worked correctly.

### Root Cause

The virtio-console was using irqfd/EventFd for interrupt delivery, which was
not functioning correctly. The guest was not receiving completion notifications
for TX queue operations, causing forked processes to hang waiting for I/O.

### Fix

Changed interrupt delivery to use direct `set_irq_line` calls with
edge-triggered behavior (assert then de-assert), matching the virtio_net
implementation which worked correctly.

### Test

```bash
cargo test-linux --test console_stress_test test_exec_pipe_diagnostic -- --nocapture
```

All command types now work:
| Test | Result |
|------|--------|
| Shell builtin (`echo`) | ✓ works |
| Subshell (`(echo hello)`) | ✓ works |
| Pipe (`echo \| cat`) | ✓ works |
| External command (`ls /`) | ✓ works |
