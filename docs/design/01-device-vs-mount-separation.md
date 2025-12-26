# Device Attachment vs Guest-Side Configuration

> **Series**: This is document 1 of 3 in the virtio-fs redesign series.
> - **[1. Device vs Mount Separation](./01-device-vs-mount-separation.md)** (this document) - API honesty and separation of concerns
> - [2. UID/GID Mapping](./02-virtio-fs-uid-mapping.md) - File ownership handling
> - [3. Capsa Sandbox](./03-capsa-sandbox.md) - Blessed environment with guaranteed features

## Executive Summary

The current `.share()` API is fundamentally broken. It implies auto-mounting at a guest path but actually:
- On KVM: ignores `guest_path` entirely, uses hardcoded tags (`share0`, `share1`), requires manual mount
- On vfkit: uses `guest_path` only for tag generation, still requires manual mount
- Nowhere: actually mounts the filesystem

This document proposes separating device attachment (universal) from guest-side mounting (Linux direct boot only), making the API honest and adding real auto-mount support.

## Current State: What The API Implies vs Reality

### The API

```rust
.share("./src", "/mnt/src", MountMode::ReadOnly)
//     ^^^^^^   ^^^^^^^^^   ^^^^^^^^^^^^^^^^^^^
//     host     guest       mode
//     path     path
```

**What users expect**: "Share `./src` from host, automatically mounted at `/mnt/src` in guest"

**What actually happens**: Device attached, user must manually mount

### KVM Backend Reality

```rust
// In vm.rs:470-475
let tag = match &share.mechanism {
    ShareMechanism::VirtioFs(cfg) => cfg.tag.clone().unwrap_or_else(|| format!("share{}", i)),
    _ => format!("share{}", i),  // guest_path COMPLETELY IGNORED
};
```

- `guest_path` parameter: **Ignored**
- Tag used: `share0`, `share1`, `share2`, ...
- Auto-mount: **No**
- What user must do:
  ```bash
  mkdir -p /mnt/wherever
  mount -t virtiofs share0 /mnt/wherever
  ```

### vfkit Backend Reality

```rust
// In vfkit.rs:91-103
let tag = match &share.mechanism {
    ShareMechanism::Auto => share.guest_path.replace('/', "_").trim_matches('_').to_string(),
    ShareMechanism::VirtioFs(cfg) => cfg.tag.clone().unwrap_or_else(|| {
        share.guest_path.replace('/', "_").trim_matches('_').to_string()
    }),
    // ...
};
```

- `guest_path` parameter: **Used only for tag generation** (`/mnt/src` → `mnt_src`)
- Auto-mount: **No**
- What user must do:
  ```bash
  mkdir -p /mnt/src
  mount -t virtiofs mnt_src /mnt/src
  ```

### Evidence: Our Own Tests

Every integration test manually mounts:

```rust
// From share_test.rs
console.exec(
    "mkdir -p /mnt/share && mount -t virtiofs share0 /mnt/share && echo MOUNT_OK",
    Duration::from_secs(10),
).await
```

## Problems

### 1. Misleading API

The `guest_path` parameter implies the filesystem will be mounted there. It isn't.

### 2. Inconsistent Behavior

- KVM ignores `guest_path`, uses `share{i}` tags
- vfkit uses `guest_path` to derive tags
- User code that works on one backend may not work on the other

### 3. No Type Safety

`.share()` is available on all `VmBuilder<B>` types, but:
- Auto-mounting would only be possible for Linux direct boot (requires cmdline/initrd modification)
- UEFI boot can only attach device, user must mount after boot

### 4. `guest_path` Parameter Is Useless

On KVM it does nothing. On vfkit it's just used for generating a tag (which could be explicit).

## Proposed Design

### Principle

1. **Separate device attachment from mounting**
2. **Make device attachment universal** (works for all boot types)
3. **Make auto-mounting Linux-direct-boot-only** (type-safe)
4. **Be honest about what requires manual intervention**

### New Types

```rust
/// Configuration for a virtio-fs device.
/// This is device-level configuration - no guest mount path.
#[derive(Debug, Clone)]
pub struct VirtioFsDevice {
    /// Host directory to share.
    pub host_path: PathBuf,

    /// Tag for guest to identify the device (max 36 chars).
    /// Used in: `mount -t virtiofs <tag> /path`
    pub tag: String,

    /// Whether guest can only read.
    pub read_only: bool,

    /// UID/GID mapping configuration.
    pub uid_gid_mapping: UidGidMapping,
}

impl VirtioFsDevice {
    pub fn new(host_path: impl Into<PathBuf>) -> Self {
        Self {
            host_path: host_path.into(),
            tag: String::new(),  // Auto-generated if empty
            read_only: false,
            uid_gid_mapping: UidGidMapping::squash_to_root(),
        }
    }

    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.tag = tag.into();
        self
    }

    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }

    pub fn uid_gid_mapping(mut self, mapping: UidGidMapping) -> Self {
        self.uid_gid_mapping = mapping;
        self
    }

    // Convenience methods
    pub fn passthrough_ownership(self) -> Self {
        self.uid_gid_mapping(UidGidMapping::passthrough())
    }
}

impl Default for VirtioFsDevice {
    fn default() -> Self {
        Self::new("")
    }
}
```

### API Changes

#### Universal: Device Attachment (All Boot Types)

```rust
impl<B: BootConfigBuilder, P> VmBuilder<B, P> {
    /// Attach a virtio-fs device to the VM.
    ///
    /// The guest must mount manually:
    /// ```bash
    /// mount -t virtiofs <tag> /path
    /// ```
    ///
    /// # Example
    /// ```rust,ignore
    /// let vm = Capsa::vm(config)
    ///     .virtio_fs(VirtioFsDevice::new("./workspace").tag("workspace"))
    ///     .build().await?;
    ///
    /// // Later, in guest:
    /// // mount -t virtiofs workspace /mnt
    /// ```
    pub fn virtio_fs(mut self, device: impl Into<VirtioFsDevice>) -> Self {
        self.virtio_fs_devices.push(device.into());
        self
    }
}
```

#### Capsa Sandbox Only: Auto-Mount Convenience

**Important**: Auto-mounting requires a capsa-controlled initrd that understands our cmdline parameters. This is NOT possible with arbitrary user-provided kernels/initrds.

See [Capsa Sandbox](./capsa-sandbox.md) for the full design.

```rust
impl<P> VmBuilder<CapsaSandboxConfig, P> {
    /// Share a directory with automatic mounting.
    ///
    /// This attaches a virtio-fs device AND configures the sandbox initrd
    /// to automatically mount it at the specified guest path.
    ///
    /// Only available for Capsa Sandbox (requires capsa-controlled initrd).
    ///
    /// # Example
    /// ```rust,ignore
    /// let vm = Capsa::sandbox()
    ///     .share("./workspace", "/mnt/workspace", MountMode::ReadWrite)
    ///     .build().await?;
    ///
    /// // Guest automatically has ./workspace mounted at /mnt/workspace
    /// ```
    pub fn share(
        self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
    ) -> Self {
        let guest_path = guest.into();
        let tag = Self::tag_from_path(&guest_path);

        let device = VirtioFsDevice::new(host)
            .tag(&tag)
            .read_only_if(mode == MountMode::ReadOnly);

        self.virtio_fs(device)
            .auto_mount(tag, guest_path)
    }

    /// Share with explicit device configuration.
    pub fn share_with_config(
        self,
        device: VirtioFsDevice,
        guest_path: impl Into<String>,
    ) -> Self {
        let guest_path = guest_path.into();
        let tag = device.tag.clone();

        self.virtio_fs(device)
            .auto_mount(tag, guest_path)
    }

    // Internal: records mount for cmdline generation
    fn auto_mount(mut self, tag: String, guest_path: String) -> Self {
        self.auto_mounts.push(AutoMount { tag, guest_path });
        self
    }

    fn tag_from_path(path: &str) -> String {
        path.replace('/', "_").trim_matches('_').to_string()
    }
}
```

### Auto-Mount Implementation

Auto-mounting is ONLY supported in `CapsaSandbox` because it requires:

1. **Custom cmdline parameter**: `capsa.mount=<tag>:<path>`
2. **Initrd support**: Script that parses cmdline and mounts

The sandbox initrd includes:

```bash
#!/bin/sh
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

**Why not raw VMs?**

Linux has no built-in cmdline support for mounting virtiofs at arbitrary paths (only for root via `root=virtiofs:<tag>`). We cannot assume user-provided initrds will understand our custom parameters.

### Internal Storage Changes

```rust
pub struct VmBuilder<B: BootConfigBuilder, P = No> {
    // Existing fields...

    /// Virtio-fs devices to attach (universal).
    pub(crate) virtio_fs_devices: Vec<VirtioFsDevice>,
}

// Linux-specific storage for auto-mounts
pub struct LinuxDirectBootConfig {
    // Existing fields...

    /// Mounts to set up automatically via initrd.
    pub auto_mounts: Vec<AutoMount>,
}

pub struct AutoMount {
    pub tag: String,
    pub guest_path: String,
}
```

### VmConfig Changes

```rust
pub struct VmConfig {
    // Replace:
    // pub shares: Vec<SharedDir>,

    // With:
    pub virtio_fs_devices: Vec<VirtioFsDevice>,

    // Only populated for Linux direct boot:
    pub auto_mounts: Vec<AutoMount>,
}
```

### Backend Changes

#### KVM Backend

```rust
// In vm.rs - simplified, no more ShareMechanism matching
for (i, device) in config.virtio_fs_devices.iter().enumerate() {
    let base = VIRTIO_FS_MMIO_BASE + (i as u64 * VIRTIO_MMIO_SIZE);
    let irq = VIRTIO_FS_IRQ + i as u32;

    // Use explicit tag or generate one
    let tag = if device.tag.is_empty() {
        format!("share{}", i)
    } else {
        device.tag.clone()
    };

    let virtio_fs = VirtioFs::new(
        device.host_path.clone(),
        tag,
        device.read_only,
        device.uid_gid_mapping.clone(),  // New: UID mapping
        vm_fd.clone(),
        irq,
    );
    // ... register device
}

// Handle auto_mounts for initrd modification (if implementing Option B)
if !config.auto_mounts.is_empty() {
    // Inject mount script into initrd
}
```

## Migration Path

### Phase 1: Add New API (Non-Breaking)

1. Add `VirtioFsDevice` type
2. Add `.virtio_fs()` method on all builders
3. Keep `.share()` working as before (deprecated)
4. Add `.share()` only on `VmBuilder<LinuxDirectBootConfig>` with proper implementation

### Phase 2: Fix Tag Generation

1. KVM: Use `guest_path` to derive tag (matching vfkit behavior) OR use explicit tag
2. Make tag derivation consistent across backends

### Phase 3: Implement Auto-Mount (Linux Direct Boot)

1. Implement initrd modification for auto-mounting
2. `.share()` now actually mounts at `guest_path`

### Phase 4: Deprecate Old API

1. Deprecate `SharedDir` type
2. Deprecate `ShareMechanism` enum (fold into `VirtioFsDevice`)
3. Remove after migration period

## Usage Examples

### UEFI Boot (Device Only, Manual Mount)

```rust
let vm = Capsa::vm(UefiBootConfig::new(disk))
    .virtio_fs(VirtioFsDevice::new("./workspace").tag("ws"))
    .console_enabled()
    .build()
    .await?;

let console = vm.console().await?;
console.wait_for("login:").await?;
console.write_line("root").await?;
console.wait_for("# ").await?;

// Manual mount required
console.exec("mkdir -p /mnt && mount -t virtiofs ws /mnt", timeout).await?;
```

### Linux Direct Boot (Device Only, Manual Mount)

```rust
// Raw kernel/initrd - we can't assume initrd supports our cmdline args
let vm = Capsa::vm(LinuxDirectBootConfig::new(kernel, initrd))
    .virtio_fs(VirtioFsDevice::new("./workspace").tag("ws"))
    .console_enabled()
    .build()
    .await?;

let console = vm.console().await?;
console.wait_for("# ").await?;

// Manual mount required - no .share() on raw VMs
console.exec("mkdir -p /mnt && mount -t virtiofs ws /mnt", timeout).await?;
```

### Capsa Sandbox (Auto-Mount)

```rust
// Sandbox uses capsa-controlled kernel/initrd with guaranteed features
let vm = Capsa::sandbox()
    .share("./workspace", "/mnt", MountMode::ReadWrite)
    .build()
    .await?;

// Wait for agent (not just console boot)
vm.wait_ready().await?;

// Already mounted! Use structured exec via agent
let result = vm.exec("ls /mnt").await?;
println!("files: {}", result.stdout);
```

### Capsa Sandbox (Explicit Device Config)

```rust
let vm = Capsa::sandbox()
    .share_with_config(
        VirtioFsDevice::new("./workspace")
            .tag("myshare")
            .passthrough_ownership(),
        "/mnt/workspace",
    )
    .build()
    .await?;
```

### Capsa Sandbox (Device Only, No Auto-Mount)

```rust
// Want device but will mount manually (e.g., conditional mounting)
let vm = Capsa::sandbox()
    .virtio_fs(VirtioFsDevice::new("./workspace").tag("ws"))
    .build()
    .await?;

// Must mount manually via agent
vm.exec("mkdir -p /mnt && mount -t virtiofs ws /mnt").await?;
```

## Testing Updates

Current tests manually mount. After this change:

**Before (raw VM with broken .share()):**
```rust
let vm = Capsa::vm(config)
    .share(&share_dir, "/mnt/share", MountMode::ReadWrite)
    .build().await?;

// Manual mount 😢 (share() didn't actually mount anything!)
console.exec(
    "mkdir -p /mnt/share && mount -t virtiofs share0 /mnt/share",
    timeout,
).await?;
```

**After (sandbox with real .share()):**
```rust
let vm = Capsa::sandbox()
    .share(&share_dir, "/mnt/share", MountMode::ReadWrite)
    .build().await?;

vm.wait_ready().await?;

// Auto-mounted! 🎉 Use structured exec via agent
let result = vm.exec("ls /mnt/share").await?;
```

**After (raw VM with honest API):**
```rust
let vm = Capsa::vm(LinuxDirectBootConfig::new(kernel, initrd))
    .virtio_fs(VirtioFsDevice::new(&share_dir).tag("share0"))
    .console_enabled()
    .build().await?;

// Honest: device only, manual mount required
console.exec(
    "mkdir -p /mnt/share && mount -t virtiofs share0 /mnt/share",
    timeout,
).await?;
```

## Open Questions

1. **Virtio-9p**: Should follow same pattern? Lower priority since virtio-fs is preferred.

2. **Tag uniqueness**: Should we validate tags are unique across devices?

3. **Max devices**: Should we limit number of virtio-fs devices? (IRQ allocation)

## Related Documents

- [UID/GID Mapping Design](./02-virtio-fs-uid-mapping.md) - `UidGidMapping` type used in `VirtioFsDevice`
- [Capsa Sandbox](./03-capsa-sandbox.md) - Blessed environment where `.share()` actually works

## Summary of Changes

| Component | Change |
|-----------|--------|
| `VirtioFsDevice` | New type for device config |
| `UidGidMapping` | New type (from UID mapping design) |
| `.virtio_fs()` | New method on all builders |
| `.share()` | Move to `VmBuilder<CapsaSandboxConfig>` only |
| `CapsaSandboxConfig` | New boot config with capsa-controlled kernel/initrd |
| `Capsa::sandbox()` | New entry point for sandboxes |
| `SharedDir` | Deprecate |
| `ShareMechanism` | Deprecate |
| `VmConfig.shares` | Replace with `virtio_fs_devices` + `auto_mounts` |
| KVM backend | Use `VirtioFsDevice` |
| vfkit backend | Use `VirtioFsDevice` |
| Tests | Migrate to sandbox for auto-mount, or use explicit `.virtio_fs()` |
