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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub root_disk: Option<DiskImage>,
}

impl LinuxDirectBootConfig {
    pub fn new(kernel: impl Into<PathBuf>, initrd: impl Into<PathBuf>) -> Self {
        Self {
            kernel: kernel.into(),
            initrd: initrd.into(),
            root_disk: None,
        }
    }

    pub fn with_root_disk(mut self, disk: impl Into<DiskImage>) -> Self {
        self.root_disk = Some(disk.into());
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
    fn serialization_without_root_disk_omits_field() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd");
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("root_disk"));
    }

    #[test]
    fn serialization_with_root_disk_includes_field() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd")
            .with_root_disk(DiskImage::new("/path/to/disk.raw"));
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("root_disk"));
    }
}
