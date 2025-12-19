use crate::types::DiskImage;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LinuxDirectBootConfig {
    pub kernel: PathBuf,
    pub initrd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk: Option<DiskImage>,
}

impl LinuxDirectBootConfig {
    pub fn new(kernel: impl Into<PathBuf>, initrd: impl Into<PathBuf>) -> Self {
        Self {
            kernel: kernel.into(),
            initrd: initrd.into(),
            disk: None,
        }
    }

    pub fn with_disk(mut self, disk: DiskImage) -> Self {
        self.disk = Some(disk);
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_creates_config_without_disk() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd");
        assert_eq!(config.kernel, PathBuf::from("/path/to/kernel"));
        assert_eq!(config.initrd, PathBuf::from("/path/to/initrd"));
        assert!(config.disk.is_none());
    }

    #[test]
    fn with_disk_adds_disk() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd")
            .with_disk(DiskImage::new("/path/to/disk.raw"));
        assert!(config.disk.is_some());
        assert_eq!(
            config.disk.as_ref().unwrap().path,
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
    fn serialization_without_disk_omits_field() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd");
        let json = serde_json::to_string(&config).unwrap();
        assert!(!json.contains("disk"));
    }

    #[test]
    fn serialization_with_disk_includes_field() {
        let config = LinuxDirectBootConfig::new("/path/to/kernel", "/path/to/initrd")
            .with_disk(DiskImage::new("/path/to/disk.raw"));
        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("disk"));
    }
}
