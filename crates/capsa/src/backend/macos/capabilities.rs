use capsa_core::{
    BackendCapabilities, BootMethodSupport, GuestOsSupport, ImageFormatSupport, NetworkModeSupport,
    ShareMechanismSupport,
};

pub fn macos_virtualization_capabilities() -> BackendCapabilities {
    BackendCapabilities {
        guest_os: GuestOsSupport { linux: true },
        boot_methods: BootMethodSupport { linux_direct: true },
        image_formats: ImageFormatSupport {
            raw: true,
            qcow2: false,
        },
        network_modes: NetworkModeSupport {
            none: true,
            nat: true,
        },
        share_mechanisms: ShareMechanismSupport {
            virtio_fs: true,
            virtio_9p: false,
        },
        max_cpus: None,
        max_memory_mb: None,
    }
}
