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
