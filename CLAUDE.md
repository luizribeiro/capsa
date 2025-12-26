# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

Capsa is a cross-platform Rust library for running virtual machines with a focus on security, simplicity, and embeddability. It abstracts hypervisor differences (KVM on Linux, Virtualization.framework on macOS) behind a unified Rust API. Primary use cases include AI coding agents, sandboxed environments, and integration testing.

## Build & Test Commands

All commands require the Nix devenv shell. Enter it with `devenv shell` or prefix commands with `devenv shell`.

### Linting
```bash
cargo fmt --all -- --check
cargo clippy-linux -- -D warnings    # Linux
cargo clippy-macos -- -D warnings    # macOS
```

### Testing

**Linux (KVM):**
```bash
cargo test-linux --lib               # Unit tests
cargo test-linux --doc               # Doc tests
cargo test-linux -- --test-threads=1 # Integration tests (requires /dev/kvm)
```

**macOS (three backends):**
```bash
cargo test-macos-vfkit --lib         # vfkit backend
cargo test-macos-native --lib        # Native Virtualization.framework
cargo test-macos-subprocess --lib    # Subprocess daemon backend
```

**Single test:**
```bash
cargo test-linux --test boot_test    # Run specific integration test
cargo test-linux test_name           # Run test matching name
```

### Building Test VMs
```bash
nix-build nix/test-vms -A x86_64 -o result-vms
nix-build nix/test-vms -A aarch64 -o result-vms
```

## Architecture

```
┌─────────────────────────────────────────────────────────┐
│                    capsa (main library)                  │
│  Public API: Capsa, LinuxVmBuilder, VmHandle, VmConsole │
├─────────────────────────────────────────────────────────┤
│               HypervisorBackend trait (internal)         │
├────────────────────────┬────────────────────────────────┤
│   Linux KVM Backend    │      macOS Backends            │
│   (capsa-linux-kvm)    │  vfkit | native | subprocess   │
└────────────────────────┴────────────────────────────────┘
              │                        │
        capsa-core              capsa-apple-*
   (shared types, traits)    (vz, vzd, vzd-ipc)
```

### Crates
- **capsa** (`crates/capsa/`) - Main library, public API, builder pattern
- **capsa-core** (`crates/core/`) - Shared types, traits, errors (re-exported by capsa)
- **capsa-cli** (`crates/cli/`) - Command-line interface
- **capsa-net** (`crates/net/`) - Userspace NAT networking (smoltcp-based)
- **capsa-linux-kvm** (`crates/linux/kvm/`) - Linux KVM backend
- **capsa-apple-vz** (`crates/apple/vz/`) - macOS Virtualization.framework bindings
- **capsa-apple-vzd** (`crates/apple/vzd/`) - macOS VM daemon (subprocess strategy)
- **capsa-apple-vzd-ipc** (`crates/apple/vzd-ipc/`) - IPC protocol for vzd

### Key Design Patterns

**Generic Factory**: `Capsa::vm(config)` returns a builder type determined by the config type.

**Backend Abstraction**: Users see `VmHandle`, backends implement `BackendVmHandle`. Internal traits allow backend evolution without breaking the public API.

**Builder Pattern**: `LinuxVmBuilder` / `UefiVmBuilder` provide fluent configuration for CPU count, memory, shared dirs, networking, console.

### Feature Flags
- `linux-kvm` - Linux KVM backend
- `vfkit` - macOS vfkit CLI backend
- `macos-native` - Direct Virtualization.framework (requires `#[apple_main::main]`)
- `macos-subprocess` - Subprocess daemon backend (recommended for macOS)
- `test-utils` - Test VM utilities

## macOS-Specific Notes

- Tests require `codesign-run` for Virtualization.framework entitlements (auto-installed in devenv)
- Virtualization.framework requires running on the main thread; the subprocess strategy avoids this constraint
- Integration tests require actual hardware (GitHub runners lack nested virtualization)

## Testing Notes

- Integration tests are in `crates/capsa/tests/`
- Test VMs are NixOS-based, built via `nix/test-vms/`
- Linux integration tests require `/dev/kvm` access
- Use `--test-threads=1` for integration tests to avoid resource contention
