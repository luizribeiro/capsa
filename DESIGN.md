# Capsa Design Document

**A cross-platform VM runtime library for secure workload isolation.**

*capsa* (Latin: "box") — A Rust library for running virtual machines with security-first defaults, designed as the runtime layer for agent frameworks and isolated compute environments.

---

## Overview

Capsa provides a unified Rust API for running virtual machines across platforms. It abstracts hypervisor differences (Virtualization.framework on macOS, KVM on Linux) behind a clean interface, with a focus on:

1. **Embeddability**: Use as a library in CLI tools, desktop apps, or servers
2. **Security by default**: Network policies, resource limits, isolated filesystems
3. **Simplicity**: Minimal API surface, sensible defaults
4. **Cross-platform**: Same code runs on macOS and Linux
5. **Testability**: First-class support for VM-based integration testing

### Primary Use Case

Capsa is the runtime layer for systems that need to execute untrusted code in isolation—AI coding agents, CI runners, sandboxed development environments. The first consumer will be **DevVM**, which will replace its current `microvm-run` shell-out with Capsa.

### What Capsa Is NOT

- **Not an image builder**: Use Nix, Packer, or similar to build images. Capsa runs them.
- **Not NixOS-specific**: Works with any Linux guest that supports virtio.
- **Not a container runtime**: If containers work for your use case, use containers.
- **Not an orchestrator**: Single-host VM management only.

---

## Architecture

```
┌─────────────────────────────────────────────────────────────┐
│                      Consumers                               │
│         (DevVM, agent frameworks, CI systems)                │
├─────────────────────────────────────────────────────────────┤
│                    Capsa Library                             │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐   │
│  │              Public API Layer                         │   │
│  │  Capsa::vm(LinuxDirectBootConfig) -> LinuxVmBuilder  │   │
│  │  VmHandle (lifecycle, console access)                 │   │
│  └──────────────────────────────────────────────────────┘   │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐   │
│  │              Console I/O Utilities                    │   │
│  │        (VmConsole, read/write, expect patterns)       │   │
│  └──────────────────────────────────────────────────────┘   │
│                                                              │
│  ┌──────────────────────────────────────────────────────┐   │
│  │    HypervisorBackend trait + BackendCapabilities      │   │
│  └──────────────────────────────────────────────────────┘   │
│           │                              │                   │
│  ┌────────▼───────┐            ┌────────▼───────┐           │
│  │  VfkitBackend  │            │ CloudHypervisor │           │
│  │    (macOS)     │            │    Backend      │           │
│  │  [subprocess]  │            │    (Linux)      │           │
│  └────────────────┘            │  [subprocess]   │           │
│                                └─────────────────┘           │
└─────────────────────────────────────────────────────────────┘
```

### Core Components

| Component | Responsibility |
|-----------|----------------|
| `Capsa` | Entry point with generic `vm()` method |
| `VmConfig` trait | Marker trait for VM configuration types |
| `VmHandle` | Public API: lifecycle control, status, console access |
| `VmConsole` | Async read/write streams for serial console interaction |
| `HypervisorBackend` | Internal trait abstracting hypervisor operations |
| `BackendCapabilities` | Declarative feature support matrix per backend |

### Key Design Principle: VmHandle vs Internal State

- **`VmHandle`**: The public API. Users interact only with this. Provides `start()`, `stop()`, `console()`, `status()`.
- **Internal backend state**: Hidden from users. The `HypervisorBackend` trait returns opaque handles that `VmHandle` wraps.

Users never see `BackendVmHandle` or other internal types. This allows backends to evolve (subprocess → native API) without breaking the public API.

---

## API Design

### Generic Factory Method

A single `vm()` method accepts any configuration type that implements `VmConfig`. The configuration type determines the returned builder type.

```rust
impl Capsa {
    /// Create a VM builder from a configuration.
    /// The configuration type determines which builder is returned.
    pub fn vm<C: VmConfig>(config: C) -> C::Builder;
}

/// Marker trait for VM configurations.
/// Each config type is associated with a specific builder.
pub trait VmConfig {
    type Builder;

    fn into_builder(self) -> Self::Builder;
}
```

### Configuration Types

Each VM type has its own configuration struct with required parameters:

```rust
/// Configuration for Linux VM with direct kernel boot.
/// This is the fastest boot method for Linux guests.
#[derive(Debug, Clone)]
pub struct LinuxDirectBootConfig {
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    pub disk: DiskImage,
}

impl VmConfig for LinuxDirectBootConfig {
    type Builder = LinuxVmBuilder;

    fn into_builder(self) -> LinuxVmBuilder {
        LinuxVmBuilder::new(self)
    }
}

// Future configurations (sketches - will need proper design when implemented):

/// Configuration for Linux VM with UEFI boot.
/// SKETCH: This is a placeholder. Actual design TBD when we implement UEFI support.
#[derive(Debug, Clone)]
pub struct LinuxUefiBootConfig {
    pub disk: DiskImage,
    // TBD: secure_boot, bootloader config, etc.
}

/// Configuration for Windows VM.
/// SKETCH: This is a placeholder. Actual design TBD when we implement Windows support.
/// Windows has no kernel cmdline - uses answer files, registry, etc.
#[derive(Debug, Clone)]
pub struct WindowsVmConfig {
    pub disk: DiskImage,
    // TBD: firmware (UEFI/BIOS), secure_boot, etc.
}

/// Configuration for macOS VM (Apple Silicon only).
/// SKETCH: This is a placeholder. Actual design TBD when we implement macOS support.
/// Uses Virtualization.framework with minimal boot configuration.
#[derive(Debug, Clone)]
pub struct MacOsVmConfig {
    // TBD: IPSW source, recovery mode, etc.
}
```

### Basic Usage

```rust
use capsa::{Capsa, LinuxDirectBootConfig, DiskImage, MountMode};
use std::time::Duration;

#[tokio::main]
async fn main() -> capsa::Result<()> {
    // Configuration struct has required params
    let config = LinuxDirectBootConfig {
        kernel: "./bzImage".into(),
        initrd: "./initrd".into(),
        disk: DiskImage::new("./rootfs.qcow2"),
    };

    // Generic vm() method, builder determined by config type
    let vm = Capsa::vm(config)
        .cpus(2)
        .memory_mb(2048)
        .share("./workspace", "/workspace", MountMode::ReadWrite)
        .console_enabled()
        .build()
        .await?;

    // Start the VM
    vm.start().await?;

    // Interact with console
    let mut console = vm.console().await?;
    console.wait_for("login:").await?;
    console.write_line("root").await?;
    console.wait_for("#").await?;
    console.write_line("echo hello from VM").await?;

    // Graceful shutdown
    vm.stop().await?;

    Ok(())
}
```

---

## Type Definitions

### Disk Image

```rust
/// Disk image configuration.
#[derive(Debug, Clone)]
pub struct DiskImage {
    pub path: PathBuf,
    pub format: ImageFormat,
}

impl DiskImage {
    /// Create with auto-detected format (from file extension).
    pub fn new(path: impl Into<PathBuf>) -> Self;

    /// Create with explicit format.
    pub fn with_format(path: impl Into<PathBuf>, format: ImageFormat) -> Self;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ImageFormat {
    #[default]
    Raw,
    Qcow2,
}
```

### Shared Directories (Linux Guests Only)

Shared directories use virtio-fs or virtio-9p, which are hypervisor-level features specific to Linux guests. This is different from network-based sharing (SMB/NFS) which operates at the OS level.

```rust
/// A shared directory from host to guest.
/// Uses virtio-fs (preferred) or virtio-9p (fallback).
/// NOTE: This is for Linux guests only. Windows/macOS guests would need
/// different mechanisms (SMB, etc.) which are OS-level, not hypervisor-level.
#[derive(Debug, Clone)]
pub struct SharedDir {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub mode: MountMode,
    pub mechanism: ShareMechanism,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum MountMode {
    #[default]
    ReadOnly,
    ReadWrite,
}

/// How directories are shared between host and guest.
/// These are hypervisor-level mechanisms (virtio), not OS-level (SMB/NFS).
#[derive(Debug, Clone)]
pub enum ShareMechanism {
    /// Auto-select best mechanism (virtio-fs if available, else virtio-9p).
    Auto,
    /// virtio-fs (best performance, Linux 5.4+ guest required).
    VirtioFs(VirtioFsConfig),
    /// virtio-9p (fallback for older kernels).
    Virtio9p(Virtio9pConfig),
}

impl Default for ShareMechanism {
    fn default() -> Self {
        Self::Auto
    }
}

#[derive(Debug, Clone, Default)]
pub struct VirtioFsConfig {
    /// Mount tag (auto-generated if not specified).
    pub tag: Option<String>,
    /// Cache mode: "auto", "always", or "none".
    pub cache: Option<String>,
}

#[derive(Debug, Clone, Default)]
pub struct Virtio9pConfig {
    /// Mount tag (auto-generated if not specified).
    pub tag: Option<String>,
    /// Message size (default: 262144 for performance).
    pub msize: Option<u32>,
}
```

**Note on SMB/NFS**: Network-based file sharing (SMB for Windows/macOS, NFS) is fundamentally different from virtio-fs/9p. SMB/NFS requires:
- Network connectivity between host and guest
- SMB/NFS server running on host
- Guest OS to connect as a client

This is an OS-level feature, not a hypervisor feature. For Windows/macOS guests, shared folders would need to be designed differently—likely as a separate abstraction from `SharedDir`. This will be properly designed when we implement non-Linux guest support.

### Network Configuration

```rust
#[derive(Debug, Clone, Default)]
pub enum NetworkMode {
    /// No network access.
    None,
    /// NAT networking (VM can reach internet).
    #[default]
    Nat,
    // Future: Policy-based networking via vsock proxy.
    // Policy(NetworkPolicy),
}
```

### Console Configuration

```rust
#[derive(Debug, Clone, Default)]
pub enum ConsoleMode {
    /// No console access.
    #[default]
    Disabled,
    /// Console enabled, Capsa manages internally and provides VmConsole.
    Enabled,
    /// Console outputs to stdout/stderr (for interactive CLI use).
    Stdio,
}
```

---

## Linux VM Builder

The primary builder for MVP. Other builders (Windows, macOS) will be designed when implemented.

```rust
pub struct LinuxVmBuilder {
    config: LinuxDirectBootConfig,
    resources: ResourceConfig,
    shares: Vec<SharedDir>,
    network: NetworkMode,
    console: ConsoleMode,
    cmdline: KernelCmdline,
    timeout: Option<Duration>,
}

impl LinuxVmBuilder {
    // Resources
    pub fn cpus(self, count: u32) -> Self;
    pub fn memory_mb(self, mb: u32) -> Self;
    pub fn timeout(self, duration: Duration) -> Self;

    // Shared directories (Linux-specific, uses virtio-fs/9p)
    pub fn share(
        self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
    ) -> Self;
    pub fn share_with_mechanism(
        self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
        mechanism: ShareMechanism,
    ) -> Self;
    pub fn shares(self, shares: impl IntoIterator<Item = SharedDir>) -> Self;

    // Network
    pub fn network(self, mode: NetworkMode) -> Self;
    pub fn no_network(self) -> Self;

    // Console
    pub fn console(self, mode: ConsoleMode) -> Self;
    pub fn console_enabled(self) -> Self;
    pub fn console_stdio(self) -> Self;

    // Kernel cmdline (Linux-specific)
    pub fn cmdline_arg(self, arg: impl Into<CmdlineArg>) -> Self;
    pub fn cmdline_args(self, args: impl IntoIterator<Item = impl Into<CmdlineArg>>) -> Self;

    /// Override the entire cmdline. Disables auto-generation.
    /// Use sparingly - prefer cmdline_arg() for additions.
    pub fn cmdline_override(self, cmdline: impl Into<String>) -> Self;

    // Build
    pub async fn build(self) -> Result<VmHandle>;
}
```

---

## Kernel Command Line (Linux Only)

The kernel cmdline is managed by a dedicated type that handles argument parsing, deduplication, and generation.

### KernelCmdline Type

```rust
// Located in: src/boot/linux/cmdline.rs

/// Manages Linux kernel command line arguments.
///
/// The cmdline can contain:
/// - Key-value pairs: `root=/dev/vda`, `console=ttyS0`
/// - Flags: `ro`, `quiet`, `debug`
/// - Complex values: `systemd.unit=rescue.target`
///
/// This type handles:
/// - Parsing arguments from strings
/// - Deduplication (last value wins for same key)
/// - Layered composition (hypervisor → boot config → user)
/// - Generation of final cmdline string
#[derive(Debug, Clone, Default)]
pub struct KernelCmdline {
    args: Vec<CmdlineArg>,
    override_value: Option<String>,
}

/// A single kernel command line argument.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CmdlineArg {
    /// Key-value pair: `key=value`
    KeyValue { key: String, value: String },
    /// Flag without value: `ro`, `quiet`
    Flag(String),
}

impl CmdlineArg {
    /// Parse from string. Returns KeyValue if contains '=', else Flag.
    pub fn parse(s: impl AsRef<str>) -> Self;

    /// Create a key-value argument.
    pub fn kv(key: impl Into<String>, value: impl Into<String>) -> Self;

    /// Create a flag argument.
    pub fn flag(name: impl Into<String>) -> Self;

    /// Get the key (for KeyValue) or the flag name.
    pub fn key(&self) -> &str;
}

impl From<&str> for CmdlineArg {
    fn from(s: &str) -> Self {
        Self::parse(s)
    }
}

impl From<String> for CmdlineArg {
    fn from(s: String) -> Self {
        Self::parse(s)
    }
}

impl KernelCmdline {
    /// Create an empty cmdline.
    pub fn new() -> Self;

    /// Add an argument. If a KeyValue with the same key exists, it's replaced.
    pub fn arg(&mut self, arg: impl Into<CmdlineArg>) -> &mut Self;

    /// Add multiple arguments.
    pub fn args(&mut self, args: impl IntoIterator<Item = impl Into<CmdlineArg>>) -> &mut Self;

    /// Set the root device argument.
    pub fn root(&mut self, device: &str) -> &mut Self {
        self.arg(CmdlineArg::kv("root", device))
    }

    /// Set the console argument.
    pub fn console(&mut self, device: &str) -> &mut Self {
        self.arg(CmdlineArg::kv("console", device))
    }

    /// Override the entire cmdline. Disables normal generation.
    pub fn override_with(&mut self, cmdline: impl Into<String>) -> &mut Self;

    /// Check if a key/flag is present.
    pub fn contains(&self, key: &str) -> bool;

    /// Get the value for a key (if it's a KeyValue).
    pub fn get(&self, key: &str) -> Option<&str>;

    /// Remove an argument by key.
    pub fn remove(&mut self, key: &str) -> Option<CmdlineArg>;

    /// Generate the final cmdline string.
    pub fn build(&self) -> String;

    /// Merge another cmdline into this one.
    /// Arguments from `other` override existing ones with the same key.
    pub fn merge(&mut self, other: &KernelCmdline) -> &mut Self;
}
```

### Cmdline Generation Flow

```rust
impl LinuxVmBuilder {
    fn generate_cmdline(&self, backend: &dyn HypervisorBackend) -> String {
        // If override is set, use it directly
        if let Some(override_cmdline) = &self.cmdline.override_value {
            return override_cmdline.clone();
        }

        let mut cmdline = KernelCmdline::new();

        // Layer 1: Hypervisor defaults
        cmdline.merge(&backend.kernel_cmdline_defaults());
        // e.g., console=hvc0, reboot=t, panic=-1

        // Layer 2: Boot config (root device)
        // TBD: How root device is determined needs more investigation.
        // For now, use hypervisor default.
        cmdline.root(&backend.default_root_device());

        // Layer 3: User-provided args (override earlier layers)
        cmdline.merge(&self.cmdline);

        cmdline.build()
    }
}
```

### Backend Cmdline Defaults

```rust
impl HypervisorBackend for VfkitBackend {
    fn kernel_cmdline_defaults(&self) -> KernelCmdline {
        let mut cmdline = KernelCmdline::new();
        cmdline.console("hvc0");     // virtio-console
        cmdline.arg("reboot=t");
        cmdline.arg("panic=-1");
        cmdline
    }

    fn default_root_device(&self) -> &str {
        "/dev/vda"  // virtio-blk
    }
}

impl HypervisorBackend for CloudHypervisorBackend {
    fn kernel_cmdline_defaults(&self) -> KernelCmdline {
        let mut cmdline = KernelCmdline::new();
        cmdline.console("ttyS0");    // serial
        cmdline.arg("reboot=t");
        cmdline.arg("panic=-1");
        cmdline
    }

    fn default_root_device(&self) -> &str {
        "/dev/vda"
    }
}
```

### Open Questions: Root Device

How the root device is specified needs more investigation:

- **Path-based** (`root=/dev/vda`): Simple but fragile if device order changes
- **Label-based** (`root=LABEL=nixos`): Stable but requires label set at image build time
- **UUID-based** (`root=UUID=xxx`): Most stable but requires UUID known at runtime

For MVP, we'll use the hypervisor's default root device path. Proper root device configuration will be designed based on real-world usage patterns.

---

## VmHandle (Public API)

This is the only type users interact with after building a VM:

```rust
/// Handle to a VM instance. This is the public API.
pub struct VmHandle {
    // Internal state hidden from users
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmStatus {
    /// VM created but not started.
    Created,
    /// VM is starting up.
    Starting,
    /// VM is running.
    Running,
    /// VM is shutting down.
    Stopping,
    /// VM has stopped.
    Stopped { exit_code: Option<i32> },
    /// VM failed.
    Failed { message: String },
}

impl VmHandle {
    /// Start the VM.
    pub async fn start(&self) -> Result<()>;

    /// Stop the VM gracefully.
    /// Sends shutdown signal, waits for grace period, then force kills.
    pub async fn stop(&self) -> Result<()>;

    /// Stop the VM with custom grace period.
    pub async fn stop_with_timeout(&self, grace_period: Duration) -> Result<()>;

    /// Force kill the VM immediately (no graceful shutdown).
    pub async fn kill(&self) -> Result<()>;

    /// Get current VM status.
    pub fn status(&self) -> VmStatus;

    /// Wait until VM exits.
    pub async fn wait(&self) -> Result<VmStatus>;

    /// Wait until VM exits, with timeout.
    pub async fn wait_timeout(&self, timeout: Duration) -> Result<Option<VmStatus>>;

    /// Get console access for reading/writing to serial console.
    /// Only available if console was enabled during build.
    pub async fn console(&self) -> Result<VmConsole>;

    /// Get the guest OS type.
    pub fn guest_os(&self) -> GuestOs;

    /// Get resource configuration.
    pub fn resources(&self) -> &ResourceConfig;
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuestOs {
    Linux,
    Windows,  // Future
    MacOs,    // Future
}

#[derive(Debug, Clone)]
pub struct ResourceConfig {
    pub cpus: u32,
    pub memory_mb: u32,
}
```

---

## Console I/O

First-class console interaction makes Capsa useful as a VM-based integration testing framework.

### VmConsole

```rust
/// Async interface for VM serial console I/O.
pub struct VmConsole {
    // Internal state
}

impl VmConsole {
    /// Split into read and write halves for concurrent use.
    pub fn split(self) -> (ConsoleReader, ConsoleWriter);

    /// Read bytes from the console.
    pub async fn read(&mut self, buf: &mut [u8]) -> Result<usize>;

    /// Read until a pattern is found. Returns all output including the pattern.
    pub async fn wait_for(&mut self, pattern: &str) -> Result<String>;

    /// Read until a pattern is found, with timeout.
    pub async fn wait_for_timeout(
        &mut self,
        pattern: &str,
        timeout: Duration,
    ) -> Result<String>;

    /// Read until one of several patterns is found.
    /// Returns (index of matched pattern, output including pattern).
    pub async fn wait_for_any(&mut self, patterns: &[&str]) -> Result<(usize, String)>;

    /// Read all available output without blocking.
    pub async fn read_available(&mut self) -> Result<String>;

    /// Write bytes to the console.
    pub async fn write(&mut self, data: &[u8]) -> Result<()>;

    /// Write a string to the console.
    pub async fn write_str(&mut self, s: &str) -> Result<()>;

    /// Write a string followed by a newline.
    pub async fn write_line(&mut self, s: &str) -> Result<()>;

    /// Send Ctrl+C (interrupt).
    pub async fn send_interrupt(&mut self) -> Result<()>;

    /// Send Ctrl+D (EOF).
    pub async fn send_eof(&mut self) -> Result<()>;
}

/// Read half of the console (implements AsyncRead).
pub struct ConsoleReader { /* ... */ }

/// Write half of the console (implements AsyncWrite).
pub struct ConsoleWriter { /* ... */ }
```

### Test Utilities

```rust
/// Convenience methods for integration testing.
impl VmConsole {
    /// Wait for login prompt, send username, wait for password/shell.
    pub async fn login(&mut self, username: &str, password: Option<&str>) -> Result<()>;

    /// Run a command and return its output.
    /// Waits for shell prompt after command completes.
    pub async fn run_command(&mut self, cmd: &str, prompt: &str) -> Result<String>;

    /// Run a command with timeout.
    pub async fn run_command_timeout(
        &mut self,
        cmd: &str,
        prompt: &str,
        timeout: Duration,
    ) -> Result<String>;
}
```

---

## HypervisorBackend Trait (Internal)

This trait is internal—users don't interact with it directly.

```rust
/// Internal trait for hypervisor implementations.
#[async_trait]
pub(crate) trait HypervisorBackend: Send + Sync {
    /// Human-readable backend name.
    fn name(&self) -> &'static str;

    /// Declare what features this backend supports.
    fn capabilities(&self) -> &BackendCapabilities;

    /// Check if this backend is available on the current system.
    fn is_available(&self) -> bool;

    /// Start a VM. Returns internal handle.
    async fn start(&self, config: &InternalVmConfig) -> Result<Box<dyn BackendVmHandle>>;

    // --- Linux cmdline helpers ---

    /// Return hypervisor-specific kernel cmdline defaults.
    fn kernel_cmdline_defaults(&self) -> KernelCmdline;

    /// Return the default root device path for this backend.
    fn default_root_device(&self) -> &str;
}

/// Internal handle to a running VM (backend-specific).
#[async_trait]
pub(crate) trait BackendVmHandle: Send + Sync {
    fn is_running(&self) -> bool;
    async fn wait(&self) -> Result<i32>;
    async fn shutdown(&self) -> Result<()>;
    async fn kill(&self) -> Result<()>;
    async fn console_stream(&self) -> Result<Option<ConsoleStream>>;
}
```

### Backend Capabilities

```rust
/// Declarative feature support for a backend.
/// NOTE: Some fields are sketches for future functionality and will need
/// proper design when implemented (marked with FUTURE).
#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    /// Supported guest operating systems.
    pub guest_os: GuestOsSupport,

    /// Supported boot methods.
    pub boot_methods: BootMethodSupport,

    /// Supported disk image formats.
    pub image_formats: ImageFormatSupport,

    /// Supported network modes.
    pub network_modes: NetworkModeSupport,

    /// Supports virtio-fs shared directories (Linux guests).
    pub virtio_fs: bool,

    /// Supports virtio-9p shared directories (Linux guests, fallback).
    pub virtio_9p: bool,

    /// Supports vsock communication.
    pub vsock: bool,

    /// Maximum vCPUs (None = no limit).
    pub max_cpus: Option<u32>,

    /// Maximum memory in MB (None = no limit).
    pub max_memory_mb: Option<u32>,

    // FUTURE: These are placeholders. Actual design TBD when implemented.
    // pub gpu_passthrough: bool,
    // pub usb_passthrough: bool,
    // pub audio: bool,
}

#[derive(Debug, Clone, Default)]
pub struct GuestOsSupport {
    pub linux: bool,
    // FUTURE: Windows and macOS support TBD
    // pub windows: bool,
    // pub macos: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BootMethodSupport {
    pub linux_direct: bool,
    // FUTURE: Other boot methods TBD
    // pub linux_uefi: bool,
    // pub windows_uefi: bool,
    // pub windows_bios: bool,
    // pub macos: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ImageFormatSupport {
    pub raw: bool,
    pub qcow2: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NetworkModeSupport {
    pub none: bool,
    pub nat: bool,
    // FUTURE: Other network modes TBD
    // pub bridged: bool,
    // pub vsock_only: bool,
}

impl BackendCapabilities {
    /// Validate configuration against capabilities.
    pub fn validate(&self, config: &InternalVmConfig) -> Result<()>;
}
```

---

## Error Types

```rust
#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("no suitable hypervisor backend available")]
    NoBackendAvailable,

    #[error("backend '{name}' is not available: {reason}")]
    BackendUnavailable { name: String, reason: String },

    #[error("feature not supported: {0}")]
    UnsupportedFeature(String),

    #[error("invalid configuration: {0}")]
    InvalidConfig(String),

    #[error("missing required configuration: {0}")]
    MissingConfig(String),

    #[error("guest OS not supported by backend: {0:?}")]
    UnsupportedGuestOs(GuestOs),

    #[error("VM failed to start: {0}")]
    StartFailed(String),

    #[error("VM is not running")]
    NotRunning,

    #[error("VM is already running")]
    AlreadyRunning,

    #[error("console not enabled for this VM")]
    ConsoleNotEnabled,

    #[error("operation timed out")]
    Timeout,

    #[error("pattern not found in console output")]
    PatternNotFound { pattern: String },

    #[error("hypervisor error: {0}")]
    Hypervisor(String),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),
}

pub type Result<T> = std::result::Result<T, Error>;
```

---

## Configuration File (CLI)

For standalone CLI use:

```toml
# linux-vm.toml

[guest]
os = "linux"
boot = "direct"

[boot]
kernel = "./bzImage"
initrd = "./initrd"

[disk]
path = "./rootfs.qcow2"
format = "qcow2"  # optional, auto-detected

[resources]
cpus = 2
memory = "2G"
timeout = "5m"

[network]
mode = "nat"

[console]
mode = "enabled"

# Shared directories (Linux guests only, uses virtio-fs)
[shares.workspace]
host = "./workspace"
guest = "/workspace"
mode = "rw"

# Optional: extra kernel cmdline args
[cmdline]
extra = ["quiet", "loglevel=3"]
```

---

## CLI

```bash
# Run Linux VM interactively
capsa run --config linux-vm.toml

# Run with console to stdio
capsa run --config linux-vm.toml --console stdio

# Override resources
capsa run --config linux-vm.toml --cpus 4 --memory 4G

# Quick Linux VM without config file
capsa run linux \
  --kernel ./bzImage \
  --initrd ./initrd \
  --disk ./rootfs.qcow2 \
  --share ./workspace:/workspace:rw \
  --console stdio

# Show available backends and their capabilities
capsa backends
capsa backends --json

# Show version
capsa version
```

---

## DevVM Integration

### Migration Path

**Current:**
```
DevVM → nix-build → microvm-run script → spawns hypervisor
```

**With Capsa:**
```
DevVM → nix-build → kernel + initrd + rootfs → Capsa → manages VM
```

### DevVM Code Changes

```rust
// src/daemon.rs - before
let mut child = Command::new(&vm.runner_path()).spawn()?;

// src/daemon.rs - after
let config = LinuxDirectBootConfig {
    kernel: nix_result.kernel.clone(),
    initrd: nix_result.initrd.clone(),
    disk: DiskImage::new(&nix_result.rootfs),
};

let vm_handle = Capsa::vm(config)
    .cpus(vm.cpus())
    .memory_mb(vm.memory_mb())
    .shares(vm.shares())
    .console_enabled()
    .cmdline_args(nix_result.extra_cmdline_args())
    .build()
    .await?;

vm_handle.start().await?;

// Console handling uses Capsa's VmConsole
let console = vm_handle.console().await?;
```

---

## Implementation Plan

### Phase 1: Core Library + Test Utilities (MVP)

**Goal**: Run a Linux VM on macOS, with console interaction for testing.

1. Project setup (Cargo workspace, CI)
2. Core types: `DiskImage`, `SharedDir`, `NetworkMode`, `ConsoleMode`
3. `VmConfig` trait and `LinuxDirectBootConfig`
4. `LinuxVmBuilder`
5. `KernelCmdline` type with parsing/generation
6. `VmHandle` with async lifecycle methods
7. `BackendCapabilities` struct
8. `HypervisorBackend` trait (internal)
9. `VfkitBackend` implementation
10. **Console I/O**: `VmConsole` with `wait_for()`, `write_line()`, etc.
11. **Test utilities**: `login()`, `run_command()`
12. Integration tests using the console API

**Deliverable**: Can run a Linux VM and write integration tests that interact via console.

### Phase 2: CLI & Config

**Goal**: Standalone CLI for manual testing and experimentation.

1. TOML configuration parsing
2. `capsa run` command
3. `capsa backends` command
4. Interactive console mode (`--console stdio`)

**Deliverable**: Can test VMs via CLI without writing Rust code.

### Phase 3: DevVM Migration

**Goal**: DevVM uses Capsa internally.

1. Add capsa dependency to DevVM
2. Simplify `nix/vm-builder.nix` to output boot artifacts
3. Update DevVM daemon to use `VmHandle`
4. Migrate console handling to `VmConsole`
5. Remove microvm-run generation

**Deliverable**: DevVM works identically but uses Capsa internally.

### Phase 4: Linux cloud-hypervisor Backend

**Goal**: Linux host support.

1. `CloudHypervisorBackend` implementation
2. virtiofsd management for virtio-fs
3. Platform detection and auto-selection
4. Document capability differences

**Deliverable**: Cross-platform Linux VM support.

### Phase 5: Security Features

**Goal**: Network policy enforcement.

1. vsock support in backends
2. Network policy proxy
3. Allowlist-based filtering
4. DNS interception

### Future Phases (Design TBD When Implemented)

- Linux UEFI boot
- Windows guest support
- macOS guest support (Virtualization.framework)
- Native hypervisor APIs (replace subprocess)
- GPU/USB passthrough

---

## Project Structure

```
capsa/
├── Cargo.toml                  # Workspace
├── crates/
│   ├── capsa/                  # Main library
│   │   ├── Cargo.toml
│   │   └── src/
│   │       ├── lib.rs          # Public API re-exports
│   │       ├── error.rs
│   │       ├── config.rs       # VmConfig trait
│   │       ├── types/
│   │       │   ├── mod.rs
│   │       │   ├── disk.rs     # DiskImage, ImageFormat
│   │       │   ├── share.rs    # SharedDir, MountMode, ShareMechanism
│   │       │   ├── network.rs  # NetworkMode
│   │       │   └── console.rs  # ConsoleMode
│   │       ├── boot/
│   │       │   ├── mod.rs
│   │       │   └── linux/
│   │       │       ├── mod.rs
│   │       │       ├── config.rs   # LinuxDirectBootConfig
│   │       │       └── cmdline.rs  # KernelCmdline
│   │       ├── builder/
│   │       │   ├── mod.rs
│   │       │   └── linux.rs    # LinuxVmBuilder
│   │       ├── handle.rs       # VmHandle (public API)
│   │       ├── console.rs      # VmConsole, ConsoleReader/Writer
│   │       ├── capabilities.rs # BackendCapabilities
│   │       └── backend/        # Internal
│   │           ├── mod.rs      # HypervisorBackend trait
│   │           ├── macos/      # macOS Virtualization.framework
│   │           └── linux/      # Linux KVM
│   └── capsa-cli/
│       ├── Cargo.toml
│       └── src/
│           └── main.rs
├── tests/
│   └── integration/
└── docs/
```

---

## Dependencies

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
async-trait = "0.1"
thiserror = "2"
which = "7"
uuid = { version = "1", features = ["v4"] }
tracing = "0.1"
serde = { version = "1", features = ["derive"] }

[target.'cfg(unix)'.dependencies]
nix = { version = "0.29", features = ["process", "signal"] }

[features]
default = []
blocking = []  # Sync API wrappers

[dev-dependencies]
tempfile = "3"
tokio-test = "0.4"
```

---

## Open Questions / Future Design Work

These items need more investigation or will be designed when their features are implemented:

1. **Root device specification**: How should users specify the root device (path, label, UUID)? For MVP, we use hypervisor defaults. Proper design TBD based on real usage patterns.

2. **Non-Linux guests**: Windows/macOS guests have fundamentally different boot and sharing mechanisms. Will be designed when implemented.

3. **Network-based sharing**: SMB/NFS for Windows/macOS guests is OS-level, not hypervisor-level. Needs separate abstraction from `SharedDir`.

4. **GPU/USB passthrough**: Marked as future in `BackendCapabilities`. Design TBD.

5. **Native hypervisor APIs**: Eventually replace subprocess with direct Virtualization.framework/KVM calls. Will require significant backend refactoring.

---

## References

- [Apple Virtualization.framework](https://developer.apple.com/documentation/virtualization) - macOS hypervisor API
- [KVM](https://www.kernel.org/doc/html/latest/virt/kvm/index.html) - Linux kernel virtual machine
- [virtiofsd](https://gitlab.com/virtio-fs/virtiofsd) - virtio-fs daemon
- [microvm.nix](https://github.com/astro/microvm.nix) - Reference for cmdline generation
- [DevVM](./devvm) - First consumer
