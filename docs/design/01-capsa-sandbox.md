# Capsa Sandbox

> **Series**: This is document 1 of 3 in the virtio-fs redesign series.
> - **[1. Capsa Sandbox](./01-capsa-sandbox.md)** (this document) - Blessed environment with guaranteed features
> - [2. Device vs Mount Separation](./02-device-vs-mount-separation.md) - API cleanup after sandbox exists
> - [3. UID/GID Mapping](./03-virtio-fs-uid-mapping.md) - File ownership handling

## Executive Summary

Introduce `CapsaSandbox`, a new VM type with a capsa-controlled kernel and initrd that provides guaranteed features:

- **Auto-mounting** of shared directories via cmdline
- **Main process support** via `.run()` for running user binaries
- **OCI container support** via `.oci()` for running containers
- **Guest agent** for structured command execution, file transfers, and health checks
- **Known environment** with predictable kernel version, modules, and capabilities

The sandbox uses a **separated architecture**: a minimal init (PID 1) handles mounting and process lifecycle, while a guest agent handles host-guest RPC communication. This separation enables running user workloads (binaries, containers) alongside the agent.

This is the foundation for fixing our broken `.share()` API. By building a controlled environment first, we have a proper destination for auto-mount functionality before cleaning up the existing APIs.

This complements the existing "raw" VM types (`LinuxDirectBootConfig`, `UefiBootConfig`) which make no assumptions about the guest.

## Motivation

### The Problem with Raw VMs

Current boot configs accept arbitrary kernels/initrds:

```rust
LinuxDirectBootConfig::new("./my-kernel", "./my-initrd")
```

We cannot make any guarantees about what's in that initrd:
- Does it have virtiofs support?
- Does it parse our cmdline args?
- Does it have a guest agent?

This is why `.share()` with auto-mount is fundamentally broken for raw VMs (see [Device vs Mount Separation](./02-device-vs-mount-separation.md)).

### The Solution: A Blessed Environment

`CapsaSandbox` uses a capsa-provided kernel and initrd where we control everything:

```rust
Capsa::sandbox()
    .share("./src", "/mnt/src", MountMode::ReadWrite)  // Actually works!
    .build()
```

Because we build the initrd, we can guarantee:
1. Virtiofs modules are loaded
2. Cmdline args like `capsa.mount=tag:path` are parsed and acted on
3. A guest agent is running and accessible via vsock

## Design

### Guest Architecture

The sandbox uses a separated architecture where init and agent are distinct processes:

```
┌─────────────────────────────────────────────────────────────────┐
│                        Guest VM                                  │
├─────────────────────────────────────────────────────────────────┤
│  capsa-sandbox-init (PID 1)                                     │
│  ├── Parse cmdline (capsa.mount=, capsa.run=, etc.)             │
│  ├── Mount virtiofs shares                                      │
│  ├── Spawn capsa-sandbox-agent                                  │
│  ├── Spawn main process (if .run() or .oci() specified)         │
│  ├── Reap zombies                                               │
│  ├── Forward signals to main process                            │
│  └── Shutdown when main process exits (or on agent shutdown)    │
│                                                                  │
│  capsa-sandbox-agent (child of init)                            │
│  └── RPC over vsock: exec, file ops, info, shutdown             │
│                                                                  │
│  Main Process (child of init, optional)                         │
│  └── User's binary, shell, or OCI container                     │
└─────────────────────────────────────────────────────────────────┘
```

**Why separate init from agent?**

- **Workload support**: Users can run binaries (`.run()`) or containers (`.oci()`) as the main process
- **Clean lifecycle**: Init manages process lifecycle; agent crash doesn't kill the workload
- **Separation of concerns**: Init does init things; agent does RPC things
- **Signal handling**: Init forwards SIGTERM to main process properly

### Boot Config Hierarchy

```
┌─────────────────────────────────────────────────────────────────┐
│                        VmBuilder<B>                              │
│  .virtio_fs()    - attach device (all boot types)               │
│  .console_enabled() - attach console (all boot types)           │
│  .network()      - configure networking (all boot types)        │
└─────────────────────────────────────────────────────────────────┘
        │                    │                      │
        ▼                    ▼                      ▼
┌───────────────┐  ┌─────────────────┐  ┌────────────────────────┐
│ UefiBootConfig│  │LinuxDirectBoot  │  │   CapsaSandboxConfig   │
│               │  │     Config      │  │                        │
│ Raw UEFI disk │  │ Raw kernel +    │  │ Capsa-controlled       │
│ No guarantees │  │ initrd          │  │ kernel + initrd        │
│               │  │ No guarantees   │  │                        │
│ .virtio_fs()  │  │ .virtio_fs()    │  │ .virtio_fs()           │
│ only          │  │ only            │  │ .share() ✓             │
│               │  │                 │  │ .run() / .oci() ✓      │
│               │  │                 │  │ .agent features ✓      │
└───────────────┘  └─────────────────┘  └────────────────────────┘
```

### API

#### Creating a Sandbox

```rust
// Run a shell - .run() is required
let vm = Capsa::sandbox()
    .run("/bin/sh", &[])
    .build()
    .await?;

// With shared directories (auto-mounted!)
let vm = Capsa::sandbox()
    .share("./workspace", "/mnt/workspace", MountMode::ReadWrite)
    .share("./data", "/mnt/data", MountMode::ReadOnly)
    .run("/bin/sh", &[])
    .build()
    .await?;

// With resources
let vm = Capsa::sandbox()
    .cpus(4)
    .memory_mb(2048)
    .share("./workspace", "/mnt", MountMode::ReadWrite)
    .network(NetworkMode::UserNat(Default::default()))
    .run("/mnt/workspace/my-app", &["--config", "/mnt/config.yaml"])
    .build()
    .await?;
```

**Note**: `.run()` or `.oci()` is required - there is no default main process.

#### Running a Main Process

```rust
// Run a specific binary as the main process
let vm = Capsa::sandbox()
    .share("./workspace", "/mnt", MountMode::ReadWrite)
    .run("/mnt/my-binary", &["--arg1", "--arg2"])
    .build()
    .await?;

vm.wait_ready().await?;

// Agent still available for auxiliary operations
let result = vm.exec("ps aux").await?;
vm.copy_from("/tmp/results.json", "./out.json").await?;

// Wait for main process to complete
let exit_code = vm.wait().await?;
```

#### Running an OCI Container

```rust
// Run an OCI container as the main process
let vm = Capsa::sandbox()
    .share("./workspace", "/mnt", MountMode::ReadWrite)
    .oci("python:3.11", &["python", "/mnt/script.py"])
    .build()
    .await?;

vm.wait_ready().await?;

// Agent available for debugging/inspection
let result = vm.exec("ps aux").await?;

// Wait for container to complete
let exit_code = vm.wait().await?;
```

#### API Constraints

`.run()` and `.oci()` are **mutually exclusive** - you can only specify one main process:

```rust
// ❌ Compile error: can't call .run() after .oci()
Capsa::sandbox()
    .oci("python:3.11", &["python"])
    .run("/bin/bash", &[])  // ERROR

// ❌ Compile error: can't call .oci() after .run()
Capsa::sandbox()
    .run("/bin/bash", &[])
    .oci("python:3.11", &["python"])  // ERROR

// ❌ Compile error: can't call .run() twice
Capsa::sandbox()
    .run("/bin/foo", &[])
    .run("/bin/bar", &[])  // ERROR
```

This is enforced at compile time using typestate pattern (see Implementation section).

#### Using the Guest Agent

```rust
let vm = Capsa::sandbox()
    .share("./workspace", "/mnt", MountMode::ReadWrite)
    .run("/bin/sh", &[])
    .build()
    .await?;

// Wait for agent to be ready (not just console boot)
vm.wait_ready().await?;

// Structured command execution
let result = vm.exec("ls -la /mnt").await?;
println!("stdout: {}", result.stdout);
println!("stderr: {}", result.stderr);
println!("exit code: {}", result.exit_code);

// File operations
vm.write_file("/mnt/input.txt", b"hello world").await?;
let contents = vm.read_file("/mnt/output.txt").await?;

// Copy files
vm.copy_to("./local/file.tar", "/tmp/file.tar").await?;
vm.copy_from("/tmp/results.json", "./local/results.json").await?;

// Environment info
let info = vm.info().await?;
println!("kernel: {}", info.kernel_version);
println!("hostname: {}", info.hostname);

// Request VM shutdown via agent
vm.shutdown().await?;
```

#### Comparison with Raw VMs

```rust
// Raw VM - manual everything
let vm = Capsa::vm(LinuxDirectBootConfig::new(kernel, initrd))
    .virtio_fs(VirtioFsDevice::new("./workspace").tag("ws"))
    .console_enabled()
    .build()
    .await?;

let console = vm.console().await?;
console.wait_for("# ").await?;
console.exec("mount -t virtiofs ws /mnt", timeout).await?;  // Manual!
console.exec("ls /mnt", timeout).await?;  // Parse output yourself

// Sandbox - batteries included
let vm = Capsa::sandbox()
    .share("./workspace", "/mnt", MountMode::ReadWrite)
    .run("/bin/sh", &[])
    .build()
    .await?;

vm.wait_ready().await?;
let result = vm.exec("ls /mnt").await?;  // Structured output
```

### Implementation

#### Crate Structure

The sandbox functionality is split across multiple crates:

```
crates/
├── capsa/                      # Main library (existing)
│   └── src/
│       └── sandbox/            # Sandbox builder, SandboxHandle
│           ├── mod.rs
│           ├── builder.rs      # VmBuilder<CapsaSandboxConfig>
│           ├── handle.rs       # SandboxHandle with agent client
│           └── config.rs       # CapsaSandboxConfig
│
├── sandbox/
│   ├── init/                   # capsa-sandbox-init binary
│   │   ├── Cargo.toml
│   │   └── src/
│   │       └── main.rs         # PID 1: mount, spawn, reap, signal
│   │
│   ├── agent/                  # capsa-sandbox-agent binary
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── main.rs         # vsock listener, RPC server
│   │       └── handlers.rs     # exec, file ops, info, shutdown
│   │
│   └── protocol/               # Shared types between host and guest
│       ├── Cargo.toml
│       └── src/
│           └── lib.rs          # ExecResult, SystemInfo, RPC definitions
```

**Why this structure?**

- `capsa-sandbox-init` and `capsa-sandbox-agent` are **statically linked binaries** that go into the initrd
- `capsa-sandbox-protocol` is shared between host (in `capsa` crate) and guest (in agent)
- Separation allows independent versioning and smaller binary sizes

#### CapsaSandboxConfig

```rust
/// Configuration for a Capsa sandbox VM.
#[derive(Debug, Clone)]
pub struct CapsaSandboxConfig {
    /// Override the default kernel (for testing/development)
    pub kernel_override: Option<PathBuf>,
    /// Override the default initrd (for testing/development)
    pub initrd_override: Option<PathBuf>,
}

/// What to run as the main process in the sandbox.
/// One of these MUST be specified - enforced by typestate.
#[derive(Debug, Clone)]
pub enum MainProcess {
    /// Run a specific binary
    Run { path: String, args: Vec<String> },
    /// Run an OCI container
    Oci { image: String, args: Vec<String> },
}
```

#### Sandbox Builder with Typestate

The builder uses typestate to enforce that `.run()` and `.oci()` are mutually exclusive:

```rust
/// Marker: no main process specified yet
pub struct NoMainProcess;
/// Marker: main process has been specified
pub struct HasMainProcess;

impl Capsa {
    /// Create a sandbox VM with capsa-controlled kernel/initrd.
    pub fn sandbox() -> SandboxBuilder<NoMainProcess> {
        SandboxBuilder::new()
    }
}

pub struct SandboxBuilder<M> {
    config: CapsaSandboxConfig,
    shares: Vec<ShareConfig>,
    main_process: MainProcess,
    _marker: PhantomData<M>,
}

impl<M> SandboxBuilder<M> {
    /// Share a directory with automatic mounting.
    pub fn share(
        mut self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
    ) -> Self {
        // Can call .share() regardless of main process state
        self.shares.push(ShareConfig { host: host.into(), guest: guest.into(), mode });
        self
    }

    /// Common builder methods available in any state
    pub fn cpus(mut self, count: u32) -> Self { ... }
    pub fn memory_mb(mut self, mb: u64) -> Self { ... }
}

impl SandboxBuilder<NoMainProcess> {
    /// Run a binary as the main process.
    ///
    /// Can only be called once, and cannot be combined with `.oci()`.
    pub fn run(
        self,
        path: impl Into<String>,
        args: &[&str],
    ) -> SandboxBuilder<HasMainProcess> {
        SandboxBuilder {
            config: self.config,
            shares: self.shares,
            main_process: MainProcess::Run {
                path: path.into(),
                args: args.iter().map(|s| s.to_string()).collect(),
            },
            _marker: PhantomData,
        }
    }

    /// Run an OCI container as the main process.
    ///
    /// Can only be called once, and cannot be combined with `.run()`.
    pub fn oci(
        self,
        image: impl Into<String>,
        args: &[&str],
    ) -> SandboxBuilder<HasMainProcess> {
        SandboxBuilder {
            config: self.config,
            shares: self.shares,
            main_process: MainProcess::Oci {
                image: image.into(),
                args: args.iter().map(|s| s.to_string()).collect(),
            },
            _marker: PhantomData,
        }
    }
}

// .build() is ONLY available after specifying a main process
impl SandboxBuilder<HasMainProcess> {
    pub async fn build(self) -> Result<SandboxHandle> {
        // Generate cmdline args, start VM, connect to agent
    }
}

// Trying to build without .run() or .oci() is a compile error:
// Capsa::sandbox().share(...).build()  // ERROR: build() not found
```

This ensures at compile time:
- `.run()` and `.oci()` can only be called from `NoMainProcess` state
- After calling either, the builder moves to `HasMainProcess` state
- `.build()` only available on `HasMainProcess` - must specify main process
- Neither `.run()` nor `.oci()` can be called again from `HasMainProcess` state

#### Kernel/Initrd Resolution

```rust
impl BootConfigBuilder for CapsaSandboxConfig {
    fn into_vm_config(
        self,
        // ... params
    ) -> (VmConfig, Option<PathBuf>) {
        // Resolve kernel path
        let kernel = self.kernel_override
            .unwrap_or_else(|| capsa_sandbox_kernel_path());

        // Resolve initrd path
        let initrd = self.initrd_override
            .unwrap_or_else(|| capsa_sandbox_initrd_path());

        // Build cmdline with auto-mount args
        let mut cmdline = default_sandbox_cmdline();
        for mount in &auto_mounts {
            cmdline.push_str(&format!(
                " capsa.mount={}:{}",
                mount.tag, mount.guest_path
            ));
        }

        // ... rest of config
    }
}

/// Returns path to bundled sandbox kernel
fn capsa_sandbox_kernel_path() -> PathBuf {
    // Options:
    // 1. Embedded in binary (increases size)
    // 2. Downloaded on first use (network dependency)
    // 3. Built by user via `capsa setup` (requires nix)
    // 4. Environment variable CAPSA_KERNEL_PATH
}
```

### Sandbox Init (`capsa-sandbox-init`)

The init binary runs as PID 1 and handles:

1. **Cmdline parsing**: Reads `/proc/cmdline` for `capsa.mount=`, `capsa.run=`, etc.
2. **Mounting**: Mounts virtiofs shares before spawning other processes
3. **Process spawning**: Starts agent and main process (if specified)
4. **Signal handling**: Forwards SIGTERM/SIGINT to main process
5. **Zombie reaping**: Calls `waitpid(-1, WNOHANG)` on SIGCHLD
6. **Shutdown**: Exits when main process exits (or on agent shutdown request)

```rust
// Simplified init logic
fn main() {
    // Parse cmdline
    let config = parse_cmdline("/proc/cmdline");

    // Mount shares
    for mount in &config.mounts {
        mount_virtiofs(&mount.tag, &mount.path);
    }

    // Spawn agent
    let agent_pid = spawn("/capsa-sandbox-agent", &[]);

    // Spawn main process (always specified via .run() or .oci())
    let main_pid = match &config.main_process {
        MainProcess::Run { path, args } => spawn(path, args),
        MainProcess::Oci { image, args } => spawn_oci(image, args),
    };

    // Event loop: handle signals, reap zombies
    loop {
        match wait_for_event() {
            Event::ChildExited(pid) if pid == main_pid => {
                // Main process exited, shutdown
                signal(agent_pid, SIGTERM);
                exit(0);
            }
            Event::Signal(SIGTERM) => {
                // Forward to main process
                signal(main_pid, SIGTERM);
            }
            Event::Sigchld => {
                // Reap zombies
                while waitpid(-1, WNOHANG) > 0 {}
            }
        }
    }
}
```

### Guest Agent (`capsa-sandbox-agent`)

The agent is a separate process that handles RPC communication with the host.

#### Communication Protocol

The agent communicates over vsock (port 52) using a simple RPC protocol:

```rust
// In crates/sandbox/protocol/src/lib.rs

/// RPC requests from host to agent
#[derive(Debug, Serialize, Deserialize)]
pub enum Request {
    Ping,
    Exec { command: String, env: HashMap<String, String> },
    ReadFile { path: String },
    WriteFile { path: String, contents: Vec<u8> },
    ListDir { path: String },
    Exists { path: String },
    Info,
    Shutdown,
}

/// RPC responses from agent to host
#[derive(Debug, Serialize, Deserialize)]
pub enum Response {
    Pong,
    Exec(ExecResult),
    ReadFile(Result<Vec<u8>, String>),
    WriteFile(Result<(), String>),
    ListDir(Result<Vec<DirEntry>, String>),
    Exists(bool),
    Info(SystemInfo),
    Shutdown(Result<(), String>),
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ExecResult {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SystemInfo {
    pub kernel_version: String,
    pub hostname: String,
    pub cpus: u32,
    pub memory_mb: u64,
    pub mounts: Vec<MountInfo>,
}
```

#### Agent Lifecycle

1. **Started by init**: Init spawns agent after mounting shares
2. **Listen on vsock**: Agent binds to vsock port 52
3. **Handle requests**: Process RPC requests from host
4. **Shutdown**: On `Shutdown` request, signal init to terminate

#### Host-Side Integration

```rust
impl SandboxHandle {
    /// Wait for the guest agent to be ready
    pub async fn wait_ready(&self) -> Result<()> {
        let deadline = Instant::now() + self.timeout;
        loop {
            match self.agent.ping().await {
                Ok(_) => return Ok(()),
                Err(_) if Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                Err(e) => return Err(Error::AgentNotReady(e)),
            }
        }
    }

    /// Execute a command in the guest
    pub async fn exec(&self, command: &str) -> Result<ExecResult> {
        self.agent.exec(command.to_string()).await
    }

    /// Request VM shutdown via agent
    pub async fn shutdown(&self) -> Result<()> {
        self.agent.shutdown().await
    }

    // ... other methods
}
```

### Initrd Contents

The capsa sandbox initrd contains:

```
/
├── init                    # capsa-sandbox-init (PID 1)
├── capsa-sandbox-agent     # Agent binary
├── bin/
│   ├── sh -> busybox
│   ├── ls -> busybox
│   ├── cat -> busybox
│   ├── mount -> busybox
│   └── busybox             # Busybox for basic utilities
├── lib/modules/            # Kernel modules (if not built-in)
│   ├── virtiofs.ko
│   ├── virtio_mmio.ko
│   └── vsock.ko
└── etc/
    └── passwd              # Minimal passwd for user ops
```

**Build requirements**:
- `capsa-sandbox-init`: Static binary (musl), ~1-2MB
- `capsa-sandbox-agent`: Static binary (musl), ~2-3MB
- `busybox`: Static binary, ~1MB
- Total initrd size: ~5-10MB (compressed)

### Kernel/Initrd Distribution

Options for providing the sandbox kernel/initrd:

#### Option A: Bundled in Crate (Not Recommended)

```rust
const KERNEL: &[u8] = include_bytes!("../sandbox/kernel");
const INITRD: &[u8] = include_bytes!("../sandbox/initrd");
```

**Pros**: No external dependencies
**Cons**: Huge binary size (~30MB+), hard to update

#### Option B: Separate Download

```rust
// On first use or `capsa setup`
async fn ensure_sandbox_assets() -> Result<PathBuf> {
    let cache_dir = dirs::cache_dir()?.join("capsa");
    let kernel = cache_dir.join("sandbox-kernel");

    if !kernel.exists() {
        download("https://releases.capsa.dev/sandbox/kernel", &kernel).await?;
    }
    Ok(kernel)
}
```

**Pros**: Small library size, easy updates
**Cons**: Network dependency, versioning complexity

#### Option C: Build Script (Recommended for Now)

```bash
# User runs once
nix-build nix/sandbox -o result-sandbox

# Or
capsa setup --sandbox
```

The sandbox assets are built alongside test VMs using the existing Nix infrastructure.

**Pros**: Leverages existing Nix setup, reproducible, no download
**Cons**: Requires Nix, build step

#### Option D: Cargo Feature + Build Script

```toml
[features]
sandbox = []  # Enables sandbox support, triggers build
```

Build script compiles sandbox assets during `cargo build`.

### NixOS Integration

The sandbox kernel/initrd can be built using NixOS:

```nix
# nix/sandbox/default.nix
{ pkgs, ... }:

let
  guestAgent = pkgs.rustPlatform.buildRustPackage {
    pname = "capsa-guest-agent";
    # ...
  };

  initrd = pkgs.makeInitrd {
    contents = [
      { source = "${guestAgent}/bin/capsa-agent"; target = "/init"; }
      { source = ./mount-shares.sh; target = "/mount-shares.sh"; }
    ];
  };

  kernel = pkgs.linux_latest;
in {
  inherit kernel initrd;
}
```

## Implementation Phases

### Phase 1: Sandbox Rust Scaffolding

**Goal**: Set up the host-side types and builder pattern.

**Tasks**:
1. Create `crates/sandbox/protocol/` with shared types (`ExecResult`, `SystemInfo`, `Request`, `Response`)
2. Create `CapsaSandboxConfig` type in `crates/capsa/src/sandbox/config.rs`
3. Create `SandboxBuilder` with typestate pattern in `crates/capsa/src/sandbox/builder.rs`
4. Add `Capsa::sandbox()` entry point
5. Move `.share()` method to `SandboxBuilder` (remove from other builders or deprecate)
6. Add `.run()` and `.oci()` methods with mutual exclusion
7. Implement cmdline generation for `capsa.mount=`, `capsa.run=`
8. Create placeholder `SandboxHandle` (can use existing `VmHandle` internally)

**Deliverables**:
- `Capsa::sandbox().share(...).run(...).build()` compiles
- Cmdline args are generated correctly
- `.share()` only available on sandbox builder

### Phase 2: Sandbox Init Binary

**Goal**: Build the init process that runs as PID 1.

**Tasks**:
1. Create `crates/sandbox/init/` crate
2. Implement cmdline parsing (`capsa.mount=tag:path`, `capsa.run=path:arg1:arg2`)
3. Implement virtiofs mounting
4. Implement main process spawning via `capsa.run=` cmdline arg
5. Implement signal handling (forward SIGTERM to main process)
6. Implement zombie reaping (SIGCHLD handler)
7. Implement shutdown logic (exit when main process exits)
8. Build as static binary (musl target)
9. Create Nix derivation for building initrd with init + busybox

**Deliverables**:
- `capsa-sandbox-init` binary that boots, mounts shares, runs a process
- Integration test: sandbox boots, mounts work, can run commands via console

### Phase 3: Basic Guest Agent

**Goal**: Add RPC communication between host and guest.

**Tasks**:
1. Create `crates/sandbox/agent/` crate
2. Implement vsock listener (port 52)
3. Implement `Ping` → `Pong` RPC (hello world)
4. Update init to spawn agent alongside main process
5. Implement host-side agent client in `SandboxHandle`
6. Implement `wait_ready()` using ping

**Deliverables**:
- `capsa-sandbox-agent` binary in initrd
- `vm.wait_ready()` works
- Integration test: `wait_ready()` succeeds after boot

### Phase 4: Agent Operations

**Goal**: Implement useful agent operations.

**Tasks**:
1. Implement `Exec` RPC (run command, return stdout/stderr/exit_code)
2. Implement `ReadFile` RPC
3. Implement `WriteFile` RPC
4. Implement `ListDir` RPC
5. Implement `Exists` RPC
6. Implement `Info` RPC (system info)
7. Add corresponding methods to `SandboxHandle`

**Deliverables**:
- `vm.exec("ls /mnt")` returns structured output
- `vm.read_file()`, `vm.write_file()` work
- Integration tests for all operations

### Phase 5: Agent Shutdown

**Goal**: Allow host to request clean VM shutdown.

**Tasks**:
1. Implement `Shutdown` RPC in agent
2. Agent signals init to terminate (via file, signal, or dedicated mechanism)
3. Init performs clean shutdown (SIGTERM to main process, wait, exit)
4. Add `vm.shutdown()` to `SandboxHandle`

**Deliverables**:
- `vm.shutdown()` cleanly terminates the VM
- Main process receives SIGTERM before VM exits

### Phase 6: OCI Container Support

**Goal**: Support running OCI containers as the main process.

**Tasks**:
1. Add OCI runtime to initrd (crun or similar minimal runtime)
2. Implement `capsa.oci=image:arg1:arg2` cmdline parsing
3. Implement container image pulling/caching strategy
4. Implement `spawn_oci()` in init
5. Handle container lifecycle (start, signal forwarding, exit)

**Deliverables**:
- `Capsa::sandbox().oci("python:3.11", &["python", "script.py"])` works
- Container runs with access to mounted shares

### Future Phases (Not in Initial Scope)

- **Phase 7**: Container image caching and pre-pulling
- **Phase 8**: Resource limits (cgroups) for main process/container
- **Phase 9**: Network namespace isolation for containers
- **Phase 10**: Multi-architecture support (aarch64)

## Relationship to Other Design Docs

### Device vs Mount Separation

- **Raw VMs**: `.virtio_fs()` only (device attachment)
- **Sandbox**: `.virtio_fs()` AND `.share()` (device + auto-mount)

The sandbox is the ONLY place where `.share()` is honest.

### UID/GID Mapping

`VirtioFsDevice` includes `UidGidMapping`, used by both raw VMs and sandboxes:

```rust
// Raw VM
.virtio_fs(VirtioFsDevice::new("./src").passthrough_ownership())

// Sandbox
.share_with_config(
    VirtioFsDevice::new("./src").squash_to_root(),
    "/mnt/src",
)
```

Default for sandbox shares: `squash_to_root()` (guest sees root ownership).

## Design Decisions

1. **RPC framework**: Use **tarpc** (consistent with other parts of capsa)

2. **Vsock port**: Fixed at **52** (defined as `const AGENT_VSOCK_PORT: u32 = 52`)

3. **Kernel version**: Use same kernel as `test-vm.nix` (iterate later)

4. **Architecture support**: Both **x86_64 and aarch64** (aarch64 testing on macOS host)

5. **Console access**: **Enabled** - sandbox exposes console for debugging

6. **OCI runtime**: **Deferred** - in-depth analysis during Phase 6 implementation

7. **Container image storage**: Images stored **in VM**; users can use virtiofs share for host-side caching

8. **Main process requirement**: **Explicit `.run()` required** - no default shell

## Open Questions

1. **Agent-to-init shutdown signaling**: Signal? Unix socket? Shared file?

## Summary

| Feature | LinuxDirectBoot | UefiBoot | CapsaSandbox |
|---------|-----------------|----------|--------------|
| Custom kernel/initrd | ✓ | N/A | Override only |
| `.virtio_fs()` | ✓ | ✓ | ✓ |
| `.share()` auto-mount | ✗ | ✗ | ✓ |
| `.run()` main process | ✗ | ✗ | ✓ |
| `.oci()` containers | ✗ | ✗ | ✓ |
| Guest agent | ✗ | ✗ | ✓ |
| `vm.exec()` structured | ✗ | ✗ | ✓ |
| File transfer | ✗ | ✗ | ✓ |
| `vm.shutdown()` | ✗ | ✗ | ✓ |
| `vm.wait_ready()` | ✗ | ✗ | ✓ |
| Known environment | ✗ | ✗ | ✓ |

## Related Documents

- [Device vs Mount Separation](./02-device-vs-mount-separation.md) - Why `.share()` only works on sandbox
- [UID/GID Mapping](./03-virtio-fs-uid-mapping.md) - `UidGidMapping` in `VirtioFsDevice`
