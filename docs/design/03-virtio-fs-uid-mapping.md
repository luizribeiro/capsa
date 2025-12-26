# Virtio-fs UID/GID Mapping

> **Series**: This is document 3 of 3 in the virtio-fs redesign series.
> - [1. Capsa Sandbox](./01-capsa-sandbox.md) - Blessed environment with guaranteed features
> - [2. Device vs Mount Separation](./02-device-vs-mount-separation.md) - API cleanup after sandbox exists
> - **[3. UID/GID Mapping](./03-virtio-fs-uid-mapping.md)** (this document) - File ownership handling

## Problem Statement

When sharing directories between host and guest via virtio-fs, file ownership (UID/GID) creates usability issues:

1. **UID mismatch**: Guest often runs as root (UID 0), but capsa process runs as a regular user (e.g., UID 1000). Files created by guest appear owned by UID 1000 on host, but show as owned by UID 1000 (not root) inside guest.

2. **chown fails**: Guest running as "root" cannot change file ownership because the host capsa process lacks CAP_CHOWN. Operations like `chown root:root file` return EPERM.

3. **Confusing mental model**: Guest thinks it's root but can't perform root operations on shared files.

## Current State

Our implementation uses **passthrough** - the guest sees real host UIDs/GIDs:

```
Host file owned by UID 1000 → Guest sees UID 1000
Guest runs chown → Fails with EPERM (capsa isn't root)
```

## Comparison with Other Implementations

| Aspect | Capsa (current) | Apple Virt.framework | virtiofsd |
|--------|-----------------|---------------------|-----------|
| UID shown to guest | Real host UID | Caller's UID | Configurable |
| `chown` | Fails (EPERM) | Silent no-op | Works with mapping |
| Multi-user | Works (UID mismatch) | Broken | Full support |
| Configuration | None | None | Rich CLI options |

### Apple Virtualization.framework

Uses **dynamic caller mapping** - files always appear owned by whoever is accessing them:

```
$ ls -la /mnt      # as uid 1000 → shows owner as 1000
$ sudo ls -la /mnt # as uid 0    → shows owner as 0 (!)
```

This is intentional by Apple. Simple for single-user VMs but breaks multi-user semantics.

### virtiofsd (Reference Implementation)

Supports multiple mapping modes via `--translate-uid` / `--translate-gid`:
- `guest:0:1000:65536` - Range mapping
- `squash-guest:0:1000:65536` - All guest UIDs → single host UID
- `map:0:1000:65536` - Bidirectional mapping
- Plus user namespace support for unprivileged operation

## Proposed Design

### API

```rust
/// Configuration for UID/GID mapping between host and guest.
#[derive(Clone, Debug, Default)]
pub struct UidGidMapping {
    pub uid: IdMapConfig,
    pub gid: IdMapConfig,
}

/// How to map a single ID type (UID or GID).
#[derive(Clone, Debug, Default)]
pub enum IdMapConfig {
    /// Pass through real host IDs (current behavior).
    #[default]
    Passthrough,

    /// Return the caller's ID for all files (Apple-style).
    /// Files always appear owned by whoever is accessing them.
    DynamicCaller,

    /// Return a fixed ID for all files.
    /// chown becomes a no-op.
    Squash(u32),
}

impl UidGidMapping {
    /// Real host UIDs/GIDs visible to guest.
    pub fn passthrough() -> Self {
        Self::default()
    }

    /// Caller always sees themselves as owner (Apple-style).
    pub fn dynamic_caller() -> Self {
        Self {
            uid: IdMapConfig::DynamicCaller,
            gid: IdMapConfig::DynamicCaller,
        }
    }

    /// All files appear owned by root in guest.
    pub fn squash_to_root() -> Self {
        Self::squash(0, 0)
    }

    /// All files appear owned by specified uid/gid in guest.
    pub fn squash(uid: u32, gid: u32) -> Self {
        Self {
            uid: IdMapConfig::Squash(uid),
            gid: IdMapConfig::Squash(gid),
        }
    }
}
```

### Behavior Matrix

| Mode | `stat()` returns | `chown()` behavior | Use case |
|------|------------------|-------------------|----------|
| Passthrough | Real host UID/GID | Passes to host (may fail) | Advanced users needing real ownership |
| DynamicCaller | Caller's UID/GID | No-op (success) | Apple parity, single-user VMs |
| Squash(id) | Fixed ID | No-op (success) | Guest expects root ownership |

### Integration with VirtioFsDevice

See [Device vs Mount Separation](./02-device-vs-mount-separation.md) for the full API redesign.

The `UidGidMapping` config belongs in `VirtioFsDevice`, the new device-level configuration type:

```rust
#[derive(Debug, Clone)]
pub struct VirtioFsDevice {
    pub host_path: PathBuf,
    pub tag: String,
    pub read_only: bool,
    pub uid_gid_mapping: UidGidMapping,  // Defaults to Squash(0,0)
}
```

Builder pattern for ergonomic configuration:

```rust
impl VirtioFsDevice {
    pub fn new(host_path: impl Into<PathBuf>) -> Self {
        Self {
            host_path: host_path.into(),
            tag: String::new(),
            read_only: false,
            uid_gid_mapping: UidGidMapping::squash_to_root(),  // Default
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

    pub fn dynamic_caller_ownership(self) -> Self {
        self.uid_gid_mapping(UidGidMapping::dynamic_caller())
    }
}
```

Usage examples:

```rust
// Device attachment only (all boot types - manual mount required)
.virtio_fs(VirtioFsDevice::new("./src").tag("src"))

// With custom UID mapping
.virtio_fs(
    VirtioFsDevice::new("./src")
        .tag("src")
        .passthrough_ownership()
)

// Capsa Sandbox only: auto-mount with defaults (squash to root)
Capsa::sandbox()
    .share("./src", "/mnt/src", MountMode::ReadOnly)

// Capsa Sandbox only: auto-mount with custom config
Capsa::sandbox()
    .share_with_config(
        VirtioFsDevice::new("./src")
            .tag("myshare")
            .uid_gid_mapping(UidGidMapping::squash(1000, 1000)),
        "/mnt/src",
    )
```

See [Capsa Sandbox](./01-capsa-sandbox.md) for why `.share()` is only available on sandboxes.

### Integration with Internal VirtioFs Device

The KVM backend's `VirtioFs` struct receives the mapping from config:

```rust
pub struct VirtioFs {
    // ... existing fields ...
    uid_gid_mapping: UidGidMapping,
}
```

### Implementation Changes

#### 1. `metadata_to_attr()` in `fuse/inode.rs`

Currently returns real host metadata. Will accept mapping config and caller info:

```rust
pub fn metadata_to_attr(
    ino: u64,
    metadata: &Metadata,
    mapping: &UidGidMapping,
    caller_uid: u32,  // From FuseInHeader
    caller_gid: u32,
) -> FuseAttr {
    let uid = match &mapping.uid {
        IdMapConfig::Passthrough => metadata.uid(),
        IdMapConfig::DynamicCaller => caller_uid,
        IdMapConfig::Squash(id) => *id,
    };
    let gid = match &mapping.gid {
        IdMapConfig::Passthrough => metadata.gid(),
        IdMapConfig::DynamicCaller => caller_gid,
        IdMapConfig::Squash(id) => *id,
    };

    FuseAttr {
        uid,
        gid,
        // ... rest unchanged ...
    }
}
```

#### 2. `handle_setattr()` in `virtio/fs.rs`

For chown operations, behavior depends on mode:

```rust
if (setattr.valid & (FATTR_UID | FATTR_GID)) != 0 {
    match (&self.uid_gid_mapping.uid, &self.uid_gid_mapping.gid) {
        (IdMapConfig::Passthrough, IdMapConfig::Passthrough) => {
            // Current behavior: attempt real chown
            let ret = unsafe { libc::chown(...) };
            if ret != 0 {
                return error_response(unique, errno_from_io(&...));
            }
        }
        _ => {
            // DynamicCaller or Squash: no-op, report success
        }
    }
}
```

#### 3. File creation (`handle_create`, `handle_mkdir`, etc.)

Files are created with host process's UID/GID regardless of mapping. The mapping only affects how ownership is *reported* to the guest, not how files are actually created on host.

## Default Behavior

**Recommendation**: Default to `Squash(0, 0)` (squash to root).

Rationale:
- Most ergonomic for capsa's primary use case (AI agents, sandboxing)
- Guest typically runs as root and expects to own shared files
- `ls -la` shows `root root` - intuitive
- `chown` works (as no-op) - no surprising EPERM errors
- Simpler mental model than DynamicCaller (where same file shows different owners to different users)

Users who need real multi-user semantics or host ownership visibility can opt into `passthrough()`.

Note: This is a behavior change from the current passthrough implementation. Existing users who depend on seeing real host UIDs should explicitly use `.passthrough_ownership()`.

## Future Extensions

### Range Mapping (deferred)

```rust
enum IdMapConfig {
    // ... existing variants ...

    /// Map ranges of IDs (virtiofsd-style).
    Map(Vec<IdMapEntry>),
}

enum IdMapEntry {
    Single { guest: u32, host: u32 },
    Range { guest_base: u32, host_base: u32, count: u32 },
}
```

This enables full multi-user support but adds complexity. Defer until there's a concrete use case.

### Per-Share Configuration

Currently all shares would use the same mapping. Could extend `SharedDir` to allow per-share configuration if needed.

## Testing

1. **Unit tests** for `metadata_to_attr()` with each mapping mode
2. **Unit tests** for `handle_setattr()` chown behavior
3. **Integration tests** verifying file ownership appears correctly in guest

## Related Documents

- [Capsa Sandbox](./01-capsa-sandbox.md) - Blessed environment where `.share()` works
- [Device vs Mount Separation](./02-device-vs-mount-separation.md) - API design for `.virtio_fs()` and `.share()`

## References

- [virtiofsd documentation](https://lib.rs/crates/virtiofsd)
- [libvirt virtiofs guide](https://libvirt.org/kbase/virtiofs.html)
- [Lima VM UID mapping issue](https://github.com/lima-vm/lima/issues/1513)
- [UTM VirtioFS UID issue](https://github.com/utmapp/UTM/issues/7103)
