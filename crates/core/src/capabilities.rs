// TODO: some of the structs here that contain booleans can probably be turned
// into sets of enums
// TODO: move this in backend and evaluate whether we really should expose this publicly

#[derive(Debug, Clone, Default)]
pub struct GuestOsSupport {
    pub linux: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BootMethodSupport {
    pub linux_direct: bool,
    pub uefi: bool,
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
}

#[derive(Debug, Clone, Default)]
pub struct ShareMechanismSupport {
    pub virtio_fs: bool,
    pub virtio_9p: bool,
}

/// Capabilities advertised by a hypervisor backend.
//
// TODO: virtio-vsock - socket-based host-to-guest communication
// TODO: virtio-rng - entropy source for guest randomness
// TODO: virtio-balloon - dynamic memory adjustment
// TODO: rosetta - run x86_64 binaries in ARM Linux VMs (Apple-only)
// TODO: virtio-gpu - graphics output for GUI VMs
// TODO: virtio-input - keyboard/mouse for GUI VMs
// TODO: bridged networking - shared/bridged network modes beyond NAT
// TODO: vm save/restore - suspend/resume VM state
// TODO: multiple disks - more than one block device
//
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
