# Capsa Crates

This directory contains the Rust crates that make up the Capsa VM runtime library.

## Directory Structure

```
crates/
  capsa/           # Main library (capsa)
  core/            # Core types and traits (capsa-core)
  cli/             # Command-line interface (capsa-cli)
  apple/           # macOS-specific crates
    vz/            # Virtualization.framework backend (capsa-apple-vz)
    vzd/           # VM daemon subprocess (capsa-apple-vzd)
    vzd-ipc/       # IPC protocol for daemon (capsa-apple-vzd-ipc)
```

## Crate Overview

```
┌─────────────────────────────────────────────────────────────────┐
│                          capsa-cli                              │
│                     (command-line interface)                    │
└─────────────────────────────────────────────────────────────────┘
                                │
                                ▼
┌─────────────────────────────────────────────────────────────────┐
│                            capsa                                │
│                    (main library + backends)                    │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │                     MacOsBackend                          │  │
│  │  ┌─────────────┐ ┌─────────────┐ ┌─────────────────────┐  │  │
│  │  │VfkitStrategy│ │Subprocess-  │ │  NativeStrategy     │  │  │
│  │  │             │ │Strategy     │ │  (delegates to      │  │  │
│  │  │             │ │             │ │  capsa-apple-vz)    │  │  │
│  │  └─────────────┘ └──────┬──────┘ └─────────────────────┘  │  │
│  └─────────────────────────┼─────────────────────────────────┘  │
└────────────────────────────┼────────────────────────────────────┘
                             │ IPC (tarpc)
                             ▼
              ┌──────────────────────────────┐
              │       capsa-apple-vzd        │
              │    (VM daemon subprocess)    │
              └──────────────┬───────────────┘
                             │
                             ▼
              ┌──────────────────────────────┐
              │       capsa-apple-vz         │
              │ (Virtualization.framework)   │
              └──────────────────────────────┘
                             │
                             ▼
              ┌──────────────────────────────┐
              │         capsa-core           │
              │   (types, traits, errors)    │
              └──────────────────────────────┘
```

## Crates

### `capsa`

The main library crate that provides the public API for running VMs.

**Features:**
- `LinuxVmBuilder` - Fluent API for configuring and launching Linux VMs
- `VmHandle` - Control running VMs (wait, shutdown, kill)
- `VmConsole` - Interact with VM console (read, write, wait for patterns)
- `VmPool` - Pre-warm and reuse VMs for faster startup
- `MacOsBackend` - Unified backend with strategy pattern for different execution modes

**Backend Strategies:**
- `VfkitStrategy` - Spawns the `vfkit` CLI tool (feature: `vfkit`)
- `SubprocessStrategy` - Spawns `capsa-apple-vzd` daemon via IPC (feature: `macos-subprocess`)
- `NativeStrategy` - Delegates to `capsa-apple-vz` (feature: `macos-native`)

See [macOS Backend Strategies](#macos-backend-strategies) below for details on when to use each.

---

### `capsa-core`

Core types, traits, and error definitions shared across all crates.

**Contents:**
- `HypervisorBackend` / `BackendVmHandle` - Backend traits
- `InternalVmConfig` - Internal VM configuration
- `LinuxDirectBootConfig` - Linux direct boot configuration
- `KernelCmdline` - Kernel command line builder
- `DiskImage`, `SharedDir`, `NetworkMode` - VM resource types
- `AsyncOwnedFd`, `AsyncPipe` - Async file descriptor wrappers
- `Error` / `Result` - Unified error types

This crate has minimal dependencies and is used by all other crates.

---

### `capsa-apple-vz`

Native macOS backend using Apple's Virtualization.framework directly.

**Why it's separate:** The `capsa-apple-vzd` daemon needs access to this backend, but having it depend on `capsa` would create a circular dependency (since `capsa` with `macos-subprocess` feature bundles `capsa-apple-vzd` as an artifact). Keeping this as a separate crate allows both to share the implementation.

**Used by:**
- `capsa` (via `NativeStrategy` delegation)
- `capsa-apple-vzd` (directly)

---

### `capsa-apple-vzd`

A daemon binary that runs VMs via Virtualization.framework in a separate process.

**Purpose:** Allows running VMs without requiring the main application to run on the main thread (a Virtualization.framework requirement). The daemon handles all VM operations and communicates with the parent process via IPC.

**Communication:** Uses `tarpc` RPC over stdin/stdout pipes with the parent process.

**When used:** When `capsa` is built with the `macos-subprocess` feature, the `SubprocessStrategy` spawns this daemon to handle VM operations.

---

### `capsa-apple-vzd-ipc`

IPC protocol definitions for communication between `capsa` and `capsa-apple-vzd`.

**Contents:**
- `VmService` - tarpc service trait defining RPC methods
- `VmHandleId`, `VmState` - IPC message types
- `PipeTransport` - Custom transport for stdin/stdout communication

---

### `capsa-cli`

Command-line interface for running VMs directly from the terminal.

**Commands:**
- `capsa run` - Run a VM with specified kernel, initrd, and options
- `capsa info` - Display available backends and capabilities

---

## Feature Flags

The `capsa` crate supports these feature flags:

| Feature | Description |
|---------|-------------|
| `vfkit` | Enable vfkit backend (spawns `vfkit` CLI) |
| `macos-subprocess` | Enable subprocess backend (spawns `capsa-apple-vzd`) |
| `macos-native` | Enable native backend (uses Virtualization.framework directly) |
| `test-utils` | Expose test utilities and VM configurations |
| `blocking` | Blocking API wrappers |

---

## macOS Backend Strategies

Apple's Virtualization.framework has a critical constraint: **all VM operations must be performed on the main thread**. This creates challenges when integrating with async Rust runtimes like Tokio, which typically occupy the main thread.

### The Problem

When using `#[tokio::main]`, Tokio's runtime takes over the main thread. Virtualization.framework calls made from Tokio worker threads will fail or behave incorrectly.

### Solutions

**1. SubprocessStrategy (`macos-subprocess` feature)**

Spawns a separate daemon process (`capsa-apple-vzd`) that:
- Uses `#[apple_main::main]` to properly manage the main thread
- Handles all Virtualization.framework calls in its own process
- Communicates with the parent via IPC (tarpc over stdin/stdout)

This allows the parent application to use `#[tokio::main]` freely while VM operations happen in the subprocess.

```rust
#[tokio::main]  // Main thread occupied by Tokio - that's fine!
async fn main() {
    // SubprocessStrategy handles VMs in a separate process
    let vm = Capsa::linux(config).build().await?;
}
```

**2. NativeStrategy (`macos-native` feature)**

Uses Virtualization.framework directly in-process, but requires the application to use `#[apple_main::main]` instead of `#[tokio::main]`. The `apple_main` crate manages the main thread for Apple framework calls while still supporting async/await.

```rust
#[apple_main::main]  // Manages main thread for Apple frameworks
async fn main() {
    // NativeStrategy can call Virtualization.framework directly
    let vm = Capsa::linux(config).build().await?;
}
```

**3. VfkitStrategy (`vfkit` feature)**

Spawns the external `vfkit` CLI tool, which handles Virtualization.framework internally. Like SubprocessStrategy, this works with any async runtime since VM operations happen in a separate process.

### Which to Choose?

| Strategy | Requires `apple_main`? | External dependency? | Performance |
|----------|------------------------|---------------------|-------------|
| `NativeStrategy` | Yes | None | Best (no IPC overhead) |
| `SubprocessStrategy` | No | Bundled (`capsa-apple-vzd`) | Good |
| `VfkitStrategy` | No | `vfkit` binary in PATH | Good |

- Use **NativeStrategy** if you control the application entry point and can use `#[apple_main::main]`
- Use **SubprocessStrategy** if you need `#[tokio::main]` or can't modify the entry point
- Use **VfkitStrategy** if you prefer using the established `vfkit` tool

---

## Dependency Graph

```
capsa-cli (cli/)
    └── capsa (capsa/)
            ├── capsa-core (core/)
            ├── capsa-apple-vz (apple/vz/, optional, macos-native feature)
            └── capsa-apple-vzd-ipc (apple/vzd-ipc/, optional, macos-subprocess feature)

capsa-apple-vzd (apple/vzd/)
    ├── capsa-core (core/)
    ├── capsa-apple-vz (apple/vz/)
    └── capsa-apple-vzd-ipc (apple/vzd-ipc/)

capsa-apple-vz (apple/vz/)
    └── capsa-core (core/)

capsa-apple-vzd-ipc (apple/vzd-ipc/)
    └── capsa-core (core/)
```
