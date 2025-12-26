# Capsa Sandbox

> **Series**: This is document 3 of 3 in the virtio-fs redesign series.
> - [1. Device vs Mount Separation](./01-device-vs-mount-separation.md) - API honesty and separation of concerns
> - [2. UID/GID Mapping](./02-virtio-fs-uid-mapping.md) - File ownership handling
> - **[3. Capsa Sandbox](./03-capsa-sandbox.md)** (this document) - Blessed environment with guaranteed features

## Executive Summary

Introduce `CapsaSandbox`, a new VM type with a capsa-controlled kernel and initrd that provides guaranteed features:

- **Auto-mounting** of shared directories via cmdline
- **Guest agent** for structured command execution, file transfers, and health checks
- **Known environment** with predictable kernel version, modules, and capabilities

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

This is why `.share()` with auto-mount is fundamentally broken for raw VMs (see [Device vs Mount Separation](./01-device-vs-mount-separation.md)).

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
│               │  │                 │  │ .agent features ✓      │
└───────────────┘  └─────────────────┘  └────────────────────────┘
```

### API

#### Creating a Sandbox

```rust
// Minimal - uses capsa's default kernel/initrd
let vm = Capsa::sandbox()
    .build()
    .await?;

// With shared directories (auto-mounted!)
let vm = Capsa::sandbox()
    .share("./workspace", "/mnt/workspace", MountMode::ReadWrite)
    .share("./data", "/mnt/data", MountMode::ReadOnly)
    .build()
    .await?;

// With resources
let vm = Capsa::sandbox()
    .cpus(4)
    .memory_mb(2048)
    .share("./workspace", "/mnt", MountMode::ReadWrite)
    .network(NetworkMode::UserNat(Default::default()))
    .build()
    .await?;
```

#### Using the Guest Agent

```rust
let vm = Capsa::sandbox()
    .share("./workspace", "/mnt", MountMode::ReadWrite)
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
    .build()
    .await?;

vm.wait_ready().await?;
let result = vm.exec("ls /mnt").await?;  // Structured output
```

### Implementation

#### CapsaSandboxConfig

```rust
/// Configuration for a Capsa sandbox VM.
///
/// Uses capsa-provided kernel and initrd with guaranteed features:
/// - Auto-mounting of shared directories
/// - Guest agent for structured operations
/// - Known kernel version and capabilities
#[derive(Debug, Clone)]
pub struct CapsaSandboxConfig {
    /// Override the default kernel (for testing/development)
    pub kernel_override: Option<PathBuf>,
    /// Override the default initrd (for testing/development)
    pub initrd_override: Option<PathBuf>,
}

impl Default for CapsaSandboxConfig {
    fn default() -> Self {
        Self {
            kernel_override: None,
            initrd_override: None,
        }
    }
}

impl CapsaSandboxConfig {
    pub fn new() -> Self {
        Self::default()
    }

    /// Override kernel (for testing)
    pub fn with_kernel(mut self, path: impl Into<PathBuf>) -> Self {
        self.kernel_override = Some(path.into());
        self
    }

    /// Override initrd (for testing)
    pub fn with_initrd(mut self, path: impl Into<PathBuf>) -> Self {
        self.initrd_override = Some(path.into());
        self
    }
}
```

#### Sandbox Builder

```rust
impl Capsa {
    /// Create a sandbox VM with capsa-controlled kernel/initrd.
    ///
    /// Sandboxes provide guaranteed features not available with raw VMs:
    /// - `.share()` with auto-mounting
    /// - Guest agent for structured command execution
    /// - Known environment
    pub fn sandbox() -> VmBuilder<CapsaSandboxConfig> {
        VmBuilder::new(CapsaSandboxConfig::default())
    }
}

impl<P> VmBuilder<CapsaSandboxConfig, P> {
    /// Share a directory with automatic mounting.
    ///
    /// Unlike raw VMs, sandboxes guarantee the share will be mounted
    /// at the specified guest path before the agent reports ready.
    pub fn share(
        self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
    ) -> Self {
        // Implementation: adds to auto_mounts, generates cmdline args
    }

    /// Share with explicit device configuration.
    pub fn share_with_config(
        self,
        device: VirtioFsDevice,
        guest_path: impl Into<String>,
    ) -> Self {
        // Implementation
    }
}
```

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

### Guest Agent

#### Communication Protocol

The guest agent communicates over vsock using tarpc (or similar RPC framework):

```rust
#[tarpc::service]
pub trait CapsaAgent {
    /// Execute a command and return structured output
    async fn exec(command: String) -> ExecResult;

    /// Execute with environment variables
    async fn exec_env(command: String, env: HashMap<String, String>) -> ExecResult;

    /// Read file contents
    async fn read_file(path: String) -> Result<Vec<u8>, AgentError>;

    /// Write file contents
    async fn write_file(path: String, contents: Vec<u8>) -> Result<(), AgentError>;

    /// Check if agent is ready
    async fn ping() -> PingResponse;

    /// Get system information
    async fn info() -> SystemInfo;

    /// List directory contents
    async fn list_dir(path: String) -> Result<Vec<DirEntry>, AgentError>;

    /// Check if path exists
    async fn exists(path: String) -> bool;
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

1. **Boot**: Kernel starts, initrd runs
2. **Mounts**: Initrd parses `capsa.mount=` args, mounts virtiofs shares
3. **Agent starts**: Guest agent starts and listens on vsock port (e.g., 52)
4. **Ready signal**: Agent sends ready signal or responds to ping
5. **Operations**: Host sends RPC requests, agent executes and responds

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
            .map_err(|e| Error::AgentError(e))
    }

    /// Read a file from the guest
    pub async fn read_file(&self, path: &str) -> Result<Vec<u8>> {
        self.agent.read_file(path.to_string()).await
            .map_err(|e| Error::AgentError(e))?
    }

    // ... other methods
}
```

### Initrd Requirements

The capsa sandbox initrd must include:

1. **Kernel modules**:
   - `virtiofs`
   - `virtio_pci` / `virtio_mmio`
   - `vsock` / `vmw_vsock_virtio_transport`

2. **Mount parsing script**:
   ```bash
   #!/bin/sh
   # /init or part of init

   # Parse capsa.mount=<tag>:<path> from cmdline
   for arg in $(cat /proc/cmdline); do
       case "$arg" in
           capsa.mount=*)
               spec="${arg#capsa.mount=}"
               tag="${spec%%:*}"
               path="${spec#*:}"
               mkdir -p "$path"
               mount -t virtiofs "$tag" "$path"
               ;;
       esac
   done
   ```

3. **Guest agent binary**: Statically linked binary that:
   - Listens on vsock port 52
   - Implements the `CapsaAgent` service
   - Runs as PID 1 or started by minimal init

4. **Minimal userspace**: busybox or similar for basic operations

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

## Migration Path

### Phase 1: Infrastructure

1. Create `CapsaSandboxConfig` type
2. Add `Capsa::sandbox()` entry point
3. Implement basic boot (kernel/initrd resolution)
4. Add `capsa.mount=` cmdline generation

### Phase 2: Initrd with Mount Support

1. Build initrd with virtiofs support
2. Add cmdline parsing script
3. Test auto-mounting works
4. `.share()` method on sandbox builder

### Phase 3: Guest Agent

1. Define agent protocol (tarpc service)
2. Implement guest-side agent
3. Build agent into initrd
4. Host-side `SandboxHandle` with agent methods

### Phase 4: Polish

1. `wait_ready()` implementation
2. File transfer methods
3. Error handling and timeouts
4. Documentation and examples

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

## Open Questions

1. **Agent protocol**: tarpc? Custom protocol? gRPC?

2. **Vsock port**: Fixed (52) or configurable?

3. **Sandbox base OS**: Minimal busybox? Alpine-based? NixOS minimal?

4. **Kernel version**: Track latest stable? LTS? Configurable?

5. **Architecture support**: x86_64 only initially? aarch64?

6. **Console access**: Should sandbox also expose console for debugging?

## Summary

| Feature | LinuxDirectBoot | UefiBoot | CapsaSandbox |
|---------|-----------------|----------|--------------|
| Custom kernel/initrd | ✓ | N/A | Override only |
| `.virtio_fs()` | ✓ | ✓ | ✓ |
| `.share()` auto-mount | ✗ | ✗ | ✓ |
| Guest agent | ✗ | ✗ | ✓ |
| `vm.exec()` structured | ✗ | ✗ | ✓ |
| File transfer | ✗ | ✗ | ✓ |
| `wait_ready()` | ✗ | ✗ | ✓ |
| Known environment | ✗ | ✗ | ✓ |

## Related Documents

- [Device vs Mount Separation](./01-device-vs-mount-separation.md) - Why `.share()` only works on sandbox
- [UID/GID Mapping](./02-virtio-fs-uid-mapping.md) - `UidGidMapping` in `VirtioFsDevice`
