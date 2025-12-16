mod console;
mod disk;
mod network;
mod share;

pub use console::ConsoleMode;
pub use disk::{DiskImage, ImageFormat};
pub use network::NetworkMode;
pub use share::{MountMode, ShareMechanism, SharedDir, Virtio9pConfig, VirtioFsConfig};

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestOs {
    Linux,
    Windows,
    MacOs,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResourceConfig {
    pub cpus: u32,
    pub memory_mb: u32,
}

impl Default for ResourceConfig {
    fn default() -> Self {
        Self {
            cpus: 1,
            memory_mb: 512,
        }
    }
}
