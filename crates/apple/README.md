# Apple Platform Crates

macOS-specific crates for running VMs using Apple's Virtualization.framework.

## Directory Structure

```
apple/
  vz/        # capsa-apple-vz      - Virtualization.framework backend
  vzd/       # capsa-apple-vzd     - VM daemon subprocess
  vzd-ipc/   # capsa-apple-vzd-ipc - IPC protocol for daemon
```

## Architecture

```
┌─────────────────────────────────────────────────────────────────┐
│                            capsa                                │
│  ┌───────────────────────────────────────────────────────────┐  │
│  │                     MacOsBackend                          │  │
│  │  ┌─────────────┐ ┌─────────────┐ ┌─────────────────────┐  │  │
│  │  │VfkitStrategy│ │Subprocess-  │ │  NativeStrategy     │  │  │
│  │  │             │ │Strategy     │ │                     │  │  │
│  │  └─────────────┘ └──────┬──────┘ └──────────┬──────────┘  │  │
│  └─────────────────────────┼───────────────────┼─────────────┘  │
└────────────────────────────┼───────────────────┼────────────────┘
                             │                   │
                    IPC (tarpc)                  │ direct
                             │                   │
                             ▼                   │
              ┌──────────────────────────────┐   │
              │       capsa-apple-vzd        │   │
              │    (VM daemon subprocess)    │   │
              └──────────────┬───────────────┘   │
                             │                   │
                             ▼                   ▼
              ┌──────────────────────────────────────┐
              │           capsa-apple-vz             │
              │     (Virtualization.framework)       │
              └──────────────────────────────────────┘
```

## Crates

### `capsa-apple-vz`

Native backend using Apple's Virtualization.framework directly.

**Why separate?** The `capsa-apple-vzd` daemon needs this backend, but depending on `capsa` would create a circular dependency (since `capsa` bundles `capsa-apple-vzd` as an artifact). This crate breaks the cycle.

### `capsa-apple-vzd`

Daemon binary that runs VMs in a separate process.

**Purpose:** Allows running VMs without requiring the main application to manage the main thread (a Virtualization.framework requirement).

**Communication:** tarpc RPC over stdin/stdout pipes.

### `capsa-apple-vzd-ipc`

Shared IPC protocol between `capsa` (client) and `capsa-apple-vzd` (server).

- `VmService` - tarpc service trait
- `VmHandleId`, `VmState` - Message types
- `PipeTransport` - stdin/stdout transport

## The Main Thread Problem

Apple's Virtualization.framework requires **all VM operations on the main thread**. This conflicts with async Rust runtimes like Tokio, which occupy the main thread.

### Solutions

#### 1. SubprocessStrategy (`macos-subprocess` feature)

Spawns `capsa-apple-vzd` in a separate process:

```rust
#[tokio::main]  // Tokio owns main thread - that's fine!
async fn main() {
    let vm = Capsa::vm(config).build().await?;
}
```

The daemon uses `#[apple_main::main]` internally to manage the main thread.

#### 2. NativeStrategy (`macos-native` feature)

Uses Virtualization.framework directly, but requires `#[apple_main::main]`:

```rust
#[apple_main::main]  // Manages main thread for Apple frameworks
async fn main() {
    let vm = Capsa::vm(config).build().await?;
}
```

#### 3. VfkitStrategy (`vfkit` feature)

Spawns the external `vfkit` CLI tool. Works with any runtime since VM operations happen in a separate process.

### Comparison

| Strategy | Requires `apple_main`? | External dependency | Performance |
|----------|------------------------|---------------------|-------------|
| `NativeStrategy` | Yes | None | Best |
| `SubprocessStrategy` | No | Bundled | Good |
| `VfkitStrategy` | No | `vfkit` in PATH | Good |

**Choose NativeStrategy** if you control the entry point.
**Choose SubprocessStrategy** if you need `#[tokio::main]`.
**Choose VfkitStrategy** if you prefer the established `vfkit` tool.

## Dependency Graph

```
capsa (with macos-native)
    └── capsa-apple-vz (vz/)
            └── capsa-core

capsa (with macos-subprocess)
    ├── capsa-apple-vzd-ipc (vzd-ipc/)
    │       └── capsa-core
    └── capsa-apple-vzd (vzd/, bundled binary)
            ├── capsa-apple-vz (vz/)
            └── capsa-apple-vzd-ipc (vzd-ipc/)
```
