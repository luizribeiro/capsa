# CAPSA CODEBASE REVIEW

### Overall Architecture: 6/10

**What's Good:**
- Clean separation between core library and platform-specific backends
- Sensible trait-based abstraction for hypervisor backends
- Async/await usage is appropriate for VM management
- Workspace structure is reasonable and modular

**What Makes Me Nervous:**
- Three different execution strategies on macOS (native, subprocess, vfkit) - this smells like code duplication waiting to happen
- Too many layers of indirection: `VmConfig` → `BootConfigBuilder` → `VmBuilder` → `HypervisorBackend` → `BackendVmHandle` → `ExecutionStrategy` → actual hypervisor calls
- The pool implementation re-invents wheels already solved by standard concurrency primitives

---

## KEY IMPROVEMENT AREAS

### 1. API Design & Ergonomics

**Problems:**
- The `VmConfig` + `BootConfigBuilder` pattern is over-engineered. You don't need marker traits for this - a simple enum would work better and be clearer
- Builder pattern has too many methods, many of which just set fields directly
- `into_vm_config` is doing too much - it should be a conversion, not validation AND conversion

**Tasks:**
- [ ] Flatten the configuration hierarchy - merge `VmConfig` and boot-specific configs into a single struct with optional fields
- [ ] Remove the `BootConfigBuilder` trait - use runtime configuration instead of compile-time trait dispatch
- [ ] Simplify builder methods - remove the excessive chaining and provide direct struct construction for common cases
- [ ] Add `From` implementations instead of dedicated `into_vm_config` methods
- [ ] Eliminate the `Poolability<No>` vs `Poolability<Yes>` type hack - use runtime flags instead

**Example cleanup:**
```rust
// Current: over-engineered
pub trait BootConfigBuilder: Clone { ... }
pub struct VmBuilder<B: BootConfigBuilder, P = No> { ... }

// Better: simple, clear
pub struct VmConfig { ... }
pub struct VmBuilder { pool_mode: bool, ... }
```

---

### 2. Error Handling (CRITICAL)

**Problems:**
- 45 TODOs in the codebase - this means you don't know what error handling should look like
- `Error::Hypervisor(String)` is a String variant - you're losing all type information
- No recovery strategies or retry logic for transient failures
- No distinction between "configuration error" (should fail fast) and "runtime error" (should attempt recovery)

**Tasks:**
- [ ] Replace `Hypervisor(String)` with specific hypervisor error types with context
- [ ] Add structured error context using `std::error::Request` or similar
- [ ] Implement exponential backoff for VM start failures
- [ ] Add a `retryable()` method to `Error` to distinguish transient vs permanent failures
- [ ] Create an error diagnostic system that suggests fixes (e.g., "KVM not found" → "Run: sudo modprobe kvm")
- [ ] Clear all TODOs - if you don't know what to do, that's a design problem

**Example:**
```rust
// Current: useless
Err(Error::Hypervisor("failed to open /dev/kvm: Permission denied".into()))

// Better: actionable
Err(Error::HypervisorAccess {
    device: "/dev/kvm".into(),
    cause: AccessError::Permission,
    hint: "Add user to kvm group: sudo usermod -aG kvm $USER",
})
```

---

### 3. Code Duplication & Abstraction Leaks

**Problems:**
- Native, subprocess, and vfkit strategies on macOS likely duplicate boot config generation
- Console buffering logic in `VmConsole::wait_for` is repeated across methods
- Capability checking is scattered across multiple files

**Tasks:**
- [ ] Consolidate boot configuration generation into a single platform-independent function with hypervisor-specific adapters
- [ ] Extract console buffer management into its own type with reusable methods
- [ ] Create a capability query system that centralizes all feature detection
- [ ] Unify VM lifecycle management (start/stop/wait) across backends into shared code

---

### 4. Unsafe Code & Safety (39 unsafe blocks)

**Problems:**
- Signal handler installation in KVM backend is global state - not thread-safe or re-entrant safe
- No audit of unsafe blocks for invariants or safety conditions
- Mixed async/sync code paths can lead to race conditions

**Tasks:**
- [ ] Document the safety invariants for every `unsafe` block with `# Safety` comments
- [ ] Replace the global signal handler with per-VM signal handling using signalfd
- [ ] Audit all unsafe blocks: prove they can't cause UB or memory corruption
- [ ] Consider replacing `unsafe` with safe abstractions from the `std::Os*` APIs where possible
- [ ] Add Miri or Loom testing for multi-threaded unsafe code

---

### 5. Resource Management

**Problems:**
- Temp file cleanup happens on VM handle drop - what if the process is killed with -9?
- No resource limits enforced on VMs beyond CPU/memory (file descriptors, disk I/O)
- Socket cleanup in vsock is manual and error-prone

**Tasks:**
- [ ] Implement "orphaned VM detection" on startup - kill VMs whose parent process no longer exists
- [ ] Add cgroups/resource control integrations for proper resource limiting
- [ ] Use `std::fs::remove_file` with proper error handling or switch to a tempdir-based approach
- [ ] Add a "cleanup orphaned resources" command that can be run after crashes
- [ ] Consider switching temp file handling to a dedicated cleanup service

---

### 6. Testing & Coverage (8 test files for ~10K LOC)

**Problems:**
Insufficient testing for a virtualization library:
- No integration tests that actually boot VMs and verify they work
- No property-based tests (e.g., "kernel cmdline should always be valid")
- No fuzzing for console parsing logic
- Tests don't verify backend capability claims

**Tasks:**
- [ ] Add real VM boot tests using QEMU's `-kernel` mode for unit tests
- [ ] Add property-based testing for `KernelCmdline` - test it generates valid input
- [ ] Fuzz console pattern matching (it's parsing user input after all)
- [ ] Add conformance tests that verify each backend implements the HypervisorBackend trait correctly
- [ ] Add longevity tests: run VMs for hours, verify no resource leaks
- [ ] Add crash recovery tests: kill parent process, verify cleanup happens

---

### 7. Performance & Scalability

**Problems:**
- VM pool spawns all VMs at startup - what if I need 1000 VMs?
- No connection pooling or reuse for vsock connections
- String concatenation in kernel cmdline is inefficient
- Console buffer creates new strings on every read instead of reusing

**Tasks:**
- [ ] Implement lazy pool initialization - spawn VMs on demand up to N, not all at once
- [ ] Add connection pooling for vsock with configurable max connections
- [ ] Use `Cow<str>` or byte buffers in kernel cmdline to avoid allocations
- [ ] Implement ring buffers for console I/O instead of String allocations
- [ ] Add metrics collection for pool hit/miss rates, VM spawn time, etc.
- [ ] Benchmark the critical path: VM spawn → console → execute command → cleanup

---

### 8. Dependency Management

**Problems:**
- Custom fork `apple-main` dependency - what happens when upstream updates?
- Too many platform-specific conditional dependencies
- No clear dependency upgrade strategy

**Tasks:**
- [ ] Either upstream your changes to `apple-main` or vendor the code
- [ ] Define a dependency policy: monthly updates with automated PRs
- [ ] Add `cargo-deny` to check for security advisories and license compliance
- [ ] Document why each dependency exists and what it's used for
- [ ] Reduce optional features - default to "it's either enabled or not" not 3 different ways

---

### 9. Documentation & Examples

**Problems:**
- DESIGN.md is 1180 lines - nobody's reading that, split it up
- No architecture diagram showing data flow
- Limited examples for edge cases (what happens on timeout, on crash, on shutdown)
- No performance characteristics documented

**Tasks:**
- [ ] Split DESIGN.md into: ARCHITECTURE.md, BACKENDS.md, TESTING.md, PERFORMANCE.md
- [ ] Add sequence diagrams showing VM lifecycle
- [ ] Document failure modes and recovery strategies
- [ ] Add example code for all error variants with suggested handling
- [ ] Document memory/CPU overhead per running VM
- [ ] Add a "Troubleshooting" guide with common problems and solutions

---

### 10. Build System & CI

**Problems:**
- CI only runs on push and PR to main branch - no branch testing in some workflows
- Nix dependency for building VMs is heavy and slow
- No artifact caching for prebuilt VM images
- No performance regression tests in CI

**Tasks:**
- [ ] Add branch protection with required CI checks
- [ ] Cache Nix derivations using GitHub Actions cache or Cachix
- [ ] Pre-build and cache VM images as artifacts
- [ ] Add performance benchmarks to CI and alert on regressions
- [ ] Add clippy with strict warnings (`-D warnings, -D clippy::all`)
- [ ] Add `cargo-nextest` for faster test runs in parallel

---

### 11. Platform Support

**Problems:**
- Linux KVM backend only supports x86_64 - what about ARM64?
- No clear story for Windows guests
- macOS version requirements are undocumented
- Hypervisor detection is fragile - just checking if binary exists

**Tasks:**
- [ ] Add ARM64 support detection and support in KVM backend
- [ ] Document minimum macOS version and API requirements
- [ ] Document Virtualization.framework version needed
- [ ] Implement "capability validation" that actually tests features (not just checks if binary exists)
- [ ] Add a `--check-system` command that validates all requirements before use

---

### 12. Logging & Observability

**Problems:**
- Tracing is used but no structured log format defined
- No correlation IDs for VM operations (hard to trace multi-VM operations)
- No metrics collection for monitoring
- No standardized log levels for different phases

**Tasks:**
- [ ] Define a log schema with required fields for each log level
- [ ] Add VM UUID tracking in all log messages
- [ ] Integrate with a metrics system (OpenTelemetry, Prometheus)
- [ ] Add log filtering by component (backend, pool, console)
- [ ] Document what logs are emitted at each level and when

---

### 13. Code Organization

**Problems:**
- `capsa_core` exports too much - should be internal implementation details
- ~10K LOC in main library - time to split into smaller crates
- Backend selection is magical - users don't know which one will be chosen

**Tasks:**
- [ ] Split into separate crates: `capsa` (public API), `capsa-core` (shared types), `capsa-macos`, `capsa-linux` (uninstallable dependencies)
- [ ] Make backend selection explicit: `Capsa::with_backend(MacOsBackend::native())`
- [ ] Add strict visibility controls - don't expose internal types
- [ ] Consider splitting `VmHandle` into smaller traits per responsibility

---

## IMMEDIATE ACTION ITEMS (Do these first)

1. **Fix the error types** - `Hypervisor(String)` is unacceptable for production code
2. **Document all unsafe blocks** or you're asking for security disasters
3. **Add real integration tests** - you cannot release a VM runtime without boot tests
4. **Remove the 45 TODOs** - either implement or delete, TBD comments are worse than no comments
5. **Define a testing strategy** - unit, integration, property, fuzzing, performance all needed

---

## WHAT YOU DID RIGHT (Keep this)

1. Async/await usage is appropriate and well-integrated with tokio
2. Builder pattern is used correctly for complex configuration
3. Console automation utilities are genuinely useful for testing
4. VM pool concept is valuable for high-throughput scenarios
5. Separation of concerns between public API (`VmHandle`) and internal implementation (`BackendVmHandle`)

---

## THE HARD TRUTH

This is a **prototype** that's masquerading as production code. It's well-designed for showing off, but it's not ready for:

- Production workloads (insufficient error handling and recovery)
- Multi-tenant environments (no resource isolation or security boundaries)
- High-scale deployments (pool doesn't scale, no metrics or observability)
- Long-term maintenance (too many conditional features, unclear upgrade path)

**You have about 6-9 months of focused work before this can be called "production-ready"** if you want it to be trustworthy, maintainable, and scalable.

But you know what? The architecture is sound, the ideas are good, and you made some solid engineering decisions. **Fix the foundations (errors, safety, tests) and build up from there.** Everything else can wait.

---

## Codebase Statistics

- **Total Rust code:** ~10,324 LOC
- **Number of files:** ~72 .rs files
- **Tests:** 8 test files (low coverage for a VM runtime)
- **Unsafe blocks:** 39 (needs audit)
- **TODOs/FIXMEs:** 45 (clear them out)
- **Unwrap/expect in non-test code:** 158 (too many)
- **Async functions:** 128
- **Nix configuration:** 1525 LOC

---

## Dependencies Snapshot

**External dependencies of concern:**
- `apple-main` (custom fork) - needs upstreaming or vendoring
- Platform-specific backends: linux-kvm, apple/vz, apple/vzd, apple/vzd-ipc
- VM infrastructure: kvm-ioctls, kvm-bindings, linux_loader, vm-device, vm_memory

**Missing:**
- Structured logging beyond basic `tracing`
- Metrics/observability stack
- Security scanning tooling
- Fuzzing integration

---

Reviewer's note: This codebase shows promise. The core ideas are solid and the async/ergonomics are well-thought-out. But you're trying to build a production VM runtime on a prototype foundation. The difference between "works on my machine" and "production-grade" is precisely what I've outlined above.

Do the hard things first - safety, errors, tests - and the rest becomes easy.

> "If you think your code is simple, you're not thinking hard enough. If you think your code is complex, you're not refactoring enough."
