# Architecture Review Feedback

## Executive Summary

This codebase shows a well-intentioned architecture with clear separation between core types, backends, and public API. However, there are significant redundancy patterns:

1. Three macOS backends doing nearly identical work with duplicated PTY handling
2. Identical capability declarations repeated across all three macOS backends
3. Nearly identical kernel cmdline defaults in all three backends
4. Duplicated IPC type definitions between core and IPC crate
5. Premature abstraction for future backends that don't exist yet

The current `ipc` branch appears to be experimenting with subprocess-based backends, which has created three parallel implementations of essentially the same functionality with minimal differentiation.

---

## 1. Redundant APIs/Functionality

### 1.1 Three macOS Backends - Massive Redundancy

**Location:**
- `crates/capsa/src/backend/vfkit.rs` (429 lines)
- `crates/capsa/src/backend/subprocess.rs` (338 lines)
- `crates/capsa-backend-native/src/lib.rs` (573 lines)

**Why they're redundant:**

All three backends:
- Target the same platform (macOS only)
- Support identical capabilities (Linux guest, direct boot, raw images, NAT networking, virtio-fs)
- Have byte-for-byte identical kernel cmdline defaults
- Return identical default root device (`"/dev/vda"`)
- Implement identical PTY handling (287 lines duplicated between vfkit.rs and subprocess.rs)

**The only differences:**
- `vfkit.rs`: Spawns `vfkit` CLI tool as subprocess
- `subprocess.rs`: Spawns `capsa-apple-vzd` daemon as subprocess
- `native.rs`: Uses Virtualization.framework APIs directly

**Consolidation proposal:**

Single backend with strategy pattern:

```rust
pub struct VirtualizationBackend {
    strategy: Box<dyn ExecutionStrategy>,
    capabilities: BackendCapabilities,
}

trait ExecutionStrategy {
    async fn spawn_vm(&self, config: &VmConfig) -> Result<ProcessHandle>;
}

struct VfkitStrategy { vfkit_path: PathBuf }
struct SubprocessStrategy { vzd_path: PathBuf }
struct NativeStrategy { /* direct API calls */ }
```

**Recommended module structure:**
```
backend/
  macos/
    mod.rs          // Single MacOsBackend
    capabilities.rs // Shared capability definition
    cmdline.rs      // Shared cmdline defaults
    pty.rs          // Shared PTY handling
    strategy/
      native.rs     // Virtualization.framework calls
      subprocess.rs // Spawn capsa-apple-vzd
      vfkit.rs      // Spawn vfkit CLI
```

**Impact:** Eliminates ~800 lines of duplicate code

---

### 1.2 Duplicated PTY Implementation - Exact Copy-Paste

**Location:**
- `crates/capsa/src/backend/vfkit.rs:211-282` (72 lines)
- `crates/capsa/src/backend/subprocess.rs:287-337` (51 lines)

The `Pty` struct and its `new()` method are character-for-character identical in both files.

**Consolidation proposal:**

```rust
// crates/capsa-core/src/pty.rs
pub struct Pty {
    pub master: OwnedFd,
    pub slave: OwnedFd,
}

impl Pty {
    pub fn new() -> std::io::Result<Self> { /* ... */ }
    pub fn into_async_master(self) -> std::io::Result<AsyncOwnedFd> { /* ... */ }
    pub fn slave_stdio(&self) -> Result<(Stdio, Stdio, Stdio)> { /* ... */ }
}
```

---

### 1.3 Identical Capability Declarations - DRY Violation

**Location:**
- `crates/capsa-backend-native/src/lib.rs:41-58`
- `crates/capsa/src/backend/vfkit.rs:35-52`
- `crates/capsa/src/backend/subprocess.rs:25-42`

All three backends construct byte-for-byte identical `BackendCapabilities`.

**Consolidation proposal:**

```rust
pub fn macos_virtualization_capabilities() -> BackendCapabilities {
    BackendCapabilities {
        guest_os: GuestOsSupport { linux: true },
        boot_methods: BootMethodSupport { linux_direct: true },
        image_formats: ImageFormatSupport { raw: true, qcow2: false },
        network_modes: NetworkModeSupport { none: true, nat: true },
        share_mechanisms: ShareMechanismSupport { virtio_fs: true, virtio_9p: false },
        max_cpus: None,
        max_memory_mb: None,
    }
}
```

---

### 1.4 Duplicated Network/Disk Config Types

**Location:**
- `crates/capsa-core/src/types/` (core types)
- `crates/capsa-apple-vzd-ipc/src/lib.rs:8-38` (IPC types)

IPC crate defines its own versions of core types:
- `capsa_apple_vzd_ipc::VmConfig` vs `capsa_core::InternalVmConfig`
- `capsa_apple_vzd_ipc::DiskConfig` vs `capsa_core::DiskImage`
- `capsa_apple_vzd_ipc::SharedDirConfig` vs `capsa_core::SharedDir`
- `capsa_apple_vzd_ipc::NetworkMode` vs `capsa_core::NetworkMode`

Then requires manual conversion in `crates/capsa-apple-vzd/src/main.rs:106-141`.

**Consolidation proposal:**

IPC should use core types directly:

```rust
// capsa-apple-vzd-ipc/Cargo.toml
[dependencies]
capsa-core = { path = "../capsa-core" }

// capsa-apple-vzd-ipc/src/lib.rs
pub use capsa_core::{InternalVmConfig, DiskImage, SharedDir, NetworkMode};
```

**Impact:** Eliminates 35 lines of struct definitions + 35 lines of conversion logic

---

### 1.5 VmConfig Trait - YAGNI Abstraction

**Location:** `crates/capsa/src/config.rs`

The DESIGN.md shows ambitious plans for multiple config types (LinuxUefiBootConfig, WindowsVmConfig, MacOsVmConfig), but:
- Only `LinuxDirectBootConfig` exists
- All three backends only support Linux + direct boot
- No timeline for other guest OSes (marked "SKETCH: This is a placeholder")

A TODO comment at `config.rs:19` already questions this:
> `// TODO: do we really need both VmConfig and InternalVmConfig? seems a bit odd tbh`

**Consolidation proposal:**

Simplify to concrete API:

```rust
impl Capsa {
    pub fn linux(config: LinuxDirectBootConfig) -> LinuxVmBuilder {
        config.into_builder()
    }
    // Add other methods WHEN implementations exist
}
```

---

## 2. Potential Simplifications

### 2.1 Pool Feature - Premature Optimization?

**Location:** `crates/capsa/src/pool/mod.rs` (279 lines)

The VM pool feature adds significant complexity:
- Custom `Poolability` marker traits with `Yes`/`No` types
- Type-level enforcement that pooled VMs can't have disks
- Background task spawning for VM replacement
- Shutdown coordination with atomic flags

**Questions:**
- Is this actually used? No evidence of pool usage in tests or examples
- Could this be a separate crate or feature-gated?

**Suggestion:** Move to optional feature flag:
```toml
[features]
default = []
vm-pools = []
```

---

### 2.2 Async Wrapper Types - Could Be Simplified

**Location:** `crates/capsa-core/src/async_fd.rs`

Two separate types that do nearly identical async file descriptor wrapping:
- `AsyncOwnedFd` - wraps single FD
- `AsyncPipe` - wraps read + write FDs separately

Both implement AsyncRead/AsyncWrite using identical `poll_read_fd` and `poll_write_fd` helpers.

---

## 3. Well-Designed Areas

### 3.1 Error Handling

**Location:** `crates/capsa-core/src/error.rs`

- Single unified error type with clear variants
- Good use of `thiserror` for ergonomics
- Comprehensive test coverage of error messages

### 3.2 Console Abstraction

**Location:** `crates/capsa/src/console.rs`

- Clean separation of concerns (read vs write via `split()`)
- Thoughtful helper methods (`wait_for`, `wait_for_any`, `login`, `run_command`)
- Good buffering strategy for pattern matching

### 3.3 Trait Design

**Location:** `crates/capsa-core/src/backend.rs`

- `HypervisorBackend` trait is well-scoped
- Clear separation between public (`VmHandle`) and internal (`BackendVmHandle`) APIs

---

## 4. Recommended Actions (Priority Order)

### Priority 1: CRITICAL - Consolidate macOS Backends

Merge three backends into one with strategy pattern.

**Migration path:**
1. Extract `Pty` struct to `backend/macos/pty.rs`
2. Extract capabilities to `macos_capabilities()` function
3. Extract cmdline defaults to `macos_cmdline_defaults()` function
4. Create single `MacOsBackend` that selects strategy at runtime
5. Delete 2 of the 3 backend files

**Estimated reduction:** 800+ lines → 400 lines + shared modules

---

### Priority 2: HIGH - Extract Shared PTY Code

Move PTY handling to `capsa-core::pty`.

---

### Priority 3: HIGH - Simplify IPC Type Duplication

Make IPC crate use core types directly. Delete `convert_config()` function.

---

### Priority 4: MEDIUM - Remove VmConfig Trait (YAGNI)

Simplify to concrete `Capsa::linux()` method. Add abstraction back when second config type exists.

---

### Priority 5: MEDIUM - Extract Capability Definition

Create `macos_virtualization_capabilities()` function.

---

### Priority 6: LOW - Consider Pool as Optional Feature

Evaluate if pools are needed; if not, make feature-gated.

---

## 5. Summary Metrics

**Current state:**
- ~3 macOS backends doing the same work
- ~287 lines of PTY code duplicated 2x
- ~35 lines of IPC types duplicated
- ~18 lines of capabilities duplicated 3x
- ~5 lines of cmdline defaults duplicated 3x

**After consolidation:**
- 1 macOS backend with strategies
- Single PTY implementation
- Direct use of core types in IPC
- Single capability definition
- Estimated 40-50% code reduction in backend layer

---

## 6. Anti-Patterns Observed

1. **Copy-paste programming:** Entire files duplicated with minor tweaks
2. **Speculative generality:** VmConfig trait for future types that don't exist
3. **Parallel hierarchies:** Three backend implementations that mirror each other
4. **Duplicate DTOs:** IPC types mirror core types, requiring conversion
5. **Feature creep:** VM pools added before basic functionality stabilized

---

## Conclusion

The `ipc` branch has created redundancy rather than consolidating shared behavior. The primary recommendation is to merge the three macOS backends immediately - they are 95% identical and provide zero architectural benefit from separation.

Follow YAGNI: build abstractions when you have two concrete cases that need them, not speculatively.
