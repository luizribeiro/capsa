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

## Crates

### `capsa`

The main library crate providing the public API for running VMs.

- `LinuxVmBuilder` - Fluent API for configuring and launching Linux VMs
- `VmHandle` - Control running VMs (wait, shutdown, kill)
- `VmConsole` - Interact with VM console (read, write, wait for patterns)
- `VmPool` - Pre-warm and reuse VMs for faster startup

### `capsa-core`

Core types, traits, and error definitions shared across all crates.

- `HypervisorBackend` / `BackendVmHandle` - Backend traits
- `VmConfig` - Internal VM configuration
- `LinuxDirectBootConfig` - Linux direct boot configuration
- `KernelCmdline` - Kernel command line builder
- `DiskImage`, `SharedDir`, `NetworkMode` - VM resource types
- `AsyncOwnedFd`, `AsyncPipe` - Async file descriptor wrappers
- `Error` / `Result` - Unified error types

### `capsa-cli`

Command-line interface for running VMs directly from the terminal.

- `capsa run` - Run a VM with specified kernel, initrd, and options
- `capsa backends` - Display available backends and capabilities

## Feature Flags

| Feature | Description |
|---------|-------------|
| `vfkit` | Enable vfkit backend (spawns `vfkit` CLI) |
| `macos-subprocess` | Enable subprocess backend (spawns `capsa-apple-vzd`) |
| `macos-native` | Enable native backend (Virtualization.framework directly) |
| `test-utils` | Expose test utilities and VM configurations |
| `blocking` | Blocking API wrappers |

## Dependency Graph

```
capsa-cli (cli/)
    └── capsa (capsa/)
            └── capsa-core (core/)
```

For macOS-specific crates and their dependencies, see [apple/README.md](apple/README.md).
