#[derive(Debug, Clone, Default)]
pub struct GuestOsSupport {
    pub linux: bool,
}

#[derive(Debug, Clone, Default)]
pub struct BootMethodSupport {
    pub linux_direct: bool,
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
pub struct BackendCapabilities {
    pub guest_os: GuestOsSupport,
    pub boot_methods: BootMethodSupport,
    pub image_formats: ImageFormatSupport,
    pub network_modes: NetworkModeSupport,
    pub virtio_fs: bool,
    pub virtio_9p: bool,
    pub vsock: bool,
    pub max_cpus: Option<u32>,
    pub max_memory_mb: Option<u32>,
}

impl BackendCapabilities {
    pub fn supports_linux(&self) -> bool {
        self.guest_os.linux
    }

    pub fn supports_linux_direct_boot(&self) -> bool {
        self.boot_methods.linux_direct
    }

    pub fn supports_raw_images(&self) -> bool {
        self.image_formats.raw
    }

    pub fn supports_qcow2_images(&self) -> bool {
        self.image_formats.qcow2
    }
}
