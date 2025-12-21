use crate::boot::KernelCmdline;
use crate::types::DiskImage;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Boot configuration for Linux VMs using direct kernel boot.
///
/// Bypasses the bootloader and boots the kernel directly, which is faster
/// and simpler for headless Linux VMs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinuxDirectBootConfig {
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    #[serde(default)]
    pub root_disk: Option<DiskImage>,
    #[serde(skip)]
    pub cmdline: KernelCmdline,
}

impl LinuxDirectBootConfig {
    pub fn new(kernel: impl Into<PathBuf>, initrd: impl Into<PathBuf>) -> Self {
        Self {
            kernel: kernel.into(),
            initrd: initrd.into(),
            root_disk: None,
            cmdline: KernelCmdline::new(),
        }
    }

    pub fn with_root_disk(mut self, disk: impl Into<DiskImage>) -> Self {
        self.root_disk = Some(disk.into());
        self
    }
}

/// Configuration for EFI variable store.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EfiVariableStore {
    pub path: PathBuf,
    #[serde(default)]
    pub create_if_missing: bool,
}

/// Boot configuration for VMs using UEFI boot.
///
/// Boots from a disk containing an EFI bootloader (e.g., GRUB, systemd-boot).
/// This is OS-agnostic and can boot Linux, Windows, BSDs, or any OS with
/// an EFI bootloader.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UefiBootConfig {
    pub disk: DiskImage,
    #[serde(default)]
    pub efi_variable_store: Option<EfiVariableStore>,
}

impl UefiBootConfig {
    pub fn new(disk: impl Into<DiskImage>) -> Self {
        Self {
            disk: disk.into(),
            efi_variable_store: None,
        }
    }

    pub fn with_efi_variable_store(mut self, path: impl Into<PathBuf>) -> Self {
        self.efi_variable_store = Some(EfiVariableStore {
            path: path.into(),
            create_if_missing: true,
        });
        self
    }

    pub fn with_existing_efi_variable_store(mut self, path: impl Into<PathBuf>) -> Self {
        self.efi_variable_store = Some(EfiVariableStore {
            path: path.into(),
            create_if_missing: false,
        });
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_config_without_root_disk() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd");
        assert_eq!(config.kernel, PathBuf::from("/path/to/kernel"));
        assert_eq!(config.initrd, PathBuf::from("/path/to/initrd"));
        assert!(config.root_disk.is_none());
    }

    #[test]
    fn with_root_disk_adds_root_disk() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd")
            .with_root_disk(DiskImage::new("/path/to/disk.raw"));
        assert!(config.root_disk.is_some());
        assert_eq!(
            config.root_disk.as_ref().unwrap().path,
            PathBuf::from("/path/to/disk.raw")
        );
    }

    #[test]
    fn serialization_roundtrip() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd");
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: LinuxDirectBootConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.kernel, config.kernel);
        assert_eq!(deserialized.initrd, config.initrd);
    }

    #[test]
    fn serialization_without_root_disk_includes_null() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd");
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("root_disk"));
    }

    #[test]
    fn serialization_with_root_disk_includes_field() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd")
            .with_root_disk(DiskImage::new("/path/to/disk.raw"));
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("root_disk"));
    }

    #[test]
    fn uefi_new_creates_config_with_disk() {
        let config = UefiBootConfig::new("/path/to/disk.raw");
        assert_eq!(config.disk.path, PathBuf::from("/path/to/disk.raw"));
        assert!(config.efi_variable_store.is_none());
    }

    #[test]
    fn uefi_with_efi_variable_store_sets_path_and_create() {
        let config = UefiBootConfig::new("/path/to/disk.raw")
            .with_efi_variable_store("/path/to/store.efivarstore");
        let store = config.efi_variable_store.unwrap();
        assert_eq!(store.path, PathBuf::from("/path/to/store.efivarstore"));
        assert!(store.create_if_missing);
    }

    #[test]
    fn uefi_with_existing_efi_variable_store_sets_no_create() {
        let config = UefiBootConfig::new("/path/to/disk.raw")
            .with_existing_efi_variable_store("/path/to/store.efivarstore");
        let store = config.efi_variable_store.unwrap();
        assert_eq!(store.path, PathBuf::from("/path/to/store.efivarstore"));
        assert!(!store.create_if_missing);
    }

    #[test]
    fn uefi_serialization_roundtrip() {
        let config = UefiBootConfig::new("/path/to/disk.raw");
        let json = serde_json::to_string(&config).unwrap();
        let deserialized: UefiBootConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.disk.path, config.disk.path);
    }

    #[test]
    fn uefi_serialization_without_efi_store_includes_null() {
        let config = UefiBootConfig::new("/path/to/disk.raw");
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("efi_variable_store"));
    }

    #[test]
    fn uefi_serialization_with_efi_store_includes_field() {
        let config = UefiBootConfig::new("/path/to/disk.raw")
            .with_efi_variable_store("/path/to/store.efivarstore");
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("efi_variable_store"));
    }
}
