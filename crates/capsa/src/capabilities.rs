/// Guest operating systems the backend can run.
#[derive(Debug, Clone, Default)]
pub struct GuestOsSupport {
    /// Can boot and run Linux guests.
    pub linux: bool,
}

/// Methods for booting a VM.
#[derive(Debug, Clone, Default)]
pub struct BootMethodSupport {
    /// Direct kernel boot: load kernel and initrd directly without a bootloader.
    pub linux_direct: bool,
}

/// Disk image formats the backend can use.
#[derive(Debug, Clone, Default)]
pub struct ImageFormatSupport {
    /// Raw disk images (.img, .raw) - uncompressed block-for-block copies.
    pub raw: bool,
    /// QCOW2 images (.qcow2) - QEMU's copy-on-write format with compression and snapshots.
    pub qcow2: bool,
}

/// Network connectivity modes.
#[derive(Debug, Clone, Default)]
pub struct NetworkModeSupport {
    /// No network - VM is completely isolated.
    pub none: bool,
    /// NAT networking - VM can reach external networks through host's connection.
    pub nat: bool,
}

/// Host-to-guest filesystem sharing mechanisms.
#[derive(Debug, Clone, Default)]
pub struct ShareMechanismSupport {
    /// VirtioFS: high-performance sharing using FUSE.
    pub virtio_fs: bool,
    /// 9P/Plan 9: legacy sharing protocol.
    pub virtio_9p: bool,
}

/// Capabilities advertised by a hypervisor backend.
// TODO: add vsock support (socket-based host-to-guest communication)
#[derive(Debug, Clone, Default)]
pub struct BackendCapabilities {
    pub guest_os: GuestOsSupport,
    pub boot_methods: BootMethodSupport,
    pub image_formats: ImageFormatSupport,
    pub network_modes: NetworkModeSupport,
    pub share_mechanisms: ShareMechanismSupport,
    /// Maximum vCPUs the backend supports. None means no known limit.
    pub max_cpus: Option<u32>,
    /// Maximum guest memory in MB. None means no known limit.
    pub max_memory_mb: Option<u32>,
}
