# Capsa Crates

This directory contains the Rust crates that make up the Capsa VM runtime library.

## Directory Structure

```
crates/
  capsa/           # Main library
  core/            # Core types and traits
  cli/             # Command-line interface
  apple/           # macOS-specific crates (see apple/README.md)
```

## Crate Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                          capsa-cli                              │
│                     (command-line interface)                    │
└────────────────────────────────┬────────────────────────────────┘
                                 │
                                 ▼
┌─────────────────────────────────────────────────────────────────┐
│                            capsa                                │
│                    (main library + backends)                    │
└────────────────────────────────┬────────────────────────────────┘
                                 │
                                 ▼
              ┌──────────────────────────────┐
              │         capsa-core           │
              │   (types, traits, errors)    │
              └──────────────────────────────┘
```

## How It Works

```
User Code                           capsa                            Backend
─────────────────────────────────────────────────────────────────────────────

LinuxDirectBootConfig ──► Capsa::vm() ──► LinuxVmBuilder
        │                                        │
        │                                        │ .cpus(2).memory_mb(1024)
        │                                        │ .console_enabled()
        │                                        ▼
        │                                    .build()
        │                                        │
        │                                        ▼
        │                              VmConfig (internal)
        │                                        │
        │                                        ▼
        │                              HypervisorBackend::start()
        │                                        │
        └────────────────────────────────────────┼───────────────────────────
                                                 ▼
                                             VmHandle ◄── BackendVmHandle
```

- **User-facing**: `LinuxDirectBootConfig` → `LinuxVmBuilder` → `VmHandle`
- **Internal**: `VmConfig` → `HypervisorBackend` → `BackendVmHandle`

The builder resolves user configuration into `VmConfig`, which backends consume.

## Crates

### `capsa`

The main library crate. **This is what users depend on.**

Re-exports user-facing types from `capsa-core` and provides:

- `Capsa` - Entry point (`Capsa::vm(config)`)
- `LinuxVmBuilder` - Fluent API for configuring VMs
- `VmHandle` - Control running VMs (wait, shutdown, kill)
- `VmConsole` - Interact with VM console (read, write, wait for patterns)
- `VmPool` - Pre-warm and reuse VMs for faster startup

### `capsa-core`

Shared types used by `capsa` and backend crates. **Users don't depend on this directly** - types are re-exported through `capsa`.

**User-facing types** (re-exported by `capsa`):

- `LinuxDirectBootConfig` - Boot configuration for Linux VMs
- `DiskImage`, `SharedDir`, `NetworkMode` - VM resource types
- `KernelCmdline` - Kernel command line builder
- `Error` / `Result` - Error types

**Internal types** (for backend implementors):

- `HypervisorBackend` / `BackendVmHandle` - Backend traits
- `VmConfig` - Resolved configuration passed to backends
- `AsyncOwnedFd`, `AsyncPipe` - Async file descriptor utilities

### `capsa-cli`

Command-line interface for running VMs directly from the terminal.

- `capsa run` - Run a VM with specified kernel, initrd, and options
- `capsa backends` - Display available backends and capabilities

## Feature Flags

| Feature | Description |
|---------|-------------|
| `macos-subprocess` | Enable macOS backend (spawns `capsa-apple-vzd` daemon) |
| `linux-kvm` | Enable Linux KVM backend |
| `test-utils` | Expose test utilities and VM configurations |
| `blocking` | Blocking API wrappers |

## Dependency Graph

```
capsa-cli (cli/)
    └── capsa (capsa/)
            └── capsa-core (core/)
```

For macOS-specific crates and their dependencies, see [apple/README.md](apple/README.md).
