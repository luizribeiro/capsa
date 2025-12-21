use crate::{
    BackendCapabilities, BootMethodSupport, DeviceSupport, GuestOsSupport, ImageFormatSupport,
    KernelCmdline, NetworkModeSupport, ShareMechanismSupport,
};

pub fn macos_virtualization_capabilities() -> BackendCapabilities {
    BackendCapabilities {
        guest_os: GuestOsSupport { linux: true },
        boot_methods: BootMethodSupport {
            linux_direct: true,
            uefi: true,
        },
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
        devices: DeviceSupport { vsock: true },
        max_cpus: None,
        max_memory_mb: None,
    }
}

pub fn macos_cmdline_defaults() -> KernelCmdline {
    let mut cmdline = KernelCmdline::new();
    cmdline.console("hvc0");
    cmdline.arg("reboot", "t");
    cmdline.arg("panic", "-1");
    cmdline
}

pub const DEFAULT_ROOT_DEVICE: &str = "/dev/vda";

#[cfg(test)]
mod tests {
    use super::*;

    mod capabilities {
        use super::*;

        #[test]
        fn supports_linux_guest() {
            let caps = macos_virtualization_capabilities();
            assert!(caps.guest_os.linux);
        }

        #[test]
        fn supports_linux_direct_boot() {
            let caps = macos_virtualization_capabilities();
            assert!(caps.boot_methods.linux_direct);
        }

        #[test]
        fn supports_uefi_boot() {
            let caps = macos_virtualization_capabilities();
            assert!(caps.boot_methods.uefi);
        }

        #[test]
        fn supports_raw_images_not_qcow2() {
            let caps = macos_virtualization_capabilities();
            assert!(caps.image_formats.raw);
            assert!(!caps.image_formats.qcow2);
        }

        #[test]
        fn supports_none_and_nat_networking() {
            let caps = macos_virtualization_capabilities();
            assert!(caps.network_modes.none);
            assert!(caps.network_modes.nat);
        }

        #[test]
        fn supports_virtio_fs_not_9p() {
            let caps = macos_virtualization_capabilities();
            assert!(caps.share_mechanisms.virtio_fs);
            assert!(!caps.share_mechanisms.virtio_9p);
        }

        #[test]
        fn no_resource_limits() {
            let caps = macos_virtualization_capabilities();
            assert!(caps.max_cpus.is_none());
            assert!(caps.max_memory_mb.is_none());
        }

        #[test]
        fn supports_vsock() {
            let caps = macos_virtualization_capabilities();
            assert!(caps.devices.vsock);
        }
    }

    mod cmdline_defaults {
        use super::*;

        #[test]
        fn sets_console_to_hvc0() {
            let cmdline = macos_cmdline_defaults();
            assert_eq!(cmdline.get("console"), Some("hvc0"));
        }

        #[test]
        fn sets_reboot_to_t() {
            let cmdline = macos_cmdline_defaults();
            assert_eq!(cmdline.get("reboot"), Some("t"));
        }

        #[test]
        fn sets_panic_to_minus_one() {
            let cmdline = macos_cmdline_defaults();
            assert_eq!(cmdline.get("panic"), Some("-1"));
        }
    }

    mod root_device {
        use super::*;

        #[test]
        fn default_is_vda() {
            assert_eq!(DEFAULT_ROOT_DEVICE, "/dev/vda");
        }
    }
}
