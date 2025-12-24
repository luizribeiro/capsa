mod disk;
mod network;
mod share;

pub use disk::{DiskImage, ImageFormat};
pub use network::{NetworkMode, UserNatConfig, UserNatConfigBuilder};
pub use share::{MountMode, ShareMechanism, SharedDir, Virtio9pConfig, VirtioFsConfig};

use serde::{Deserialize, Serialize};

// TODO: support more guest operating systems
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum GuestOs {
    Linux,
}

/// Host platform the backend runs on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HostPlatform {
    MacOs,
    Linux,
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

#[cfg(test)]
mod tests {
    use super::*;

    mod resource_config {
        use super::*;

        #[test]
        fn default_values() {
            let config = ResourceConfig::default();
            assert_eq!(config.cpus, 1);
            assert_eq!(config.memory_mb, 512);
        }

        #[test]
        fn serialization_roundtrip() {
            let config = ResourceConfig {
                cpus: 4,
                memory_mb: 2048,
            };
            let json = serde_json::to_string(&config).unwrap();
            let deserialized: ResourceConfig = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.cpus, config.cpus);
            assert_eq!(deserialized.memory_mb, config.memory_mb);
        }
    }

    mod guest_os {
        use super::*;

        #[test]
        fn serializes_lowercase() {
            assert_eq!(serde_json::to_string(&GuestOs::Linux).unwrap(), "\"linux\"");
        }

        #[test]
        fn deserializes_lowercase() {
            assert_eq!(
                serde_json::from_str::<GuestOs>("\"linux\"").unwrap(),
                GuestOs::Linux
            );
        }
    }

    mod host_platform {
        use super::*;

        #[test]
        fn serializes_lowercase() {
            assert_eq!(
                serde_json::to_string(&HostPlatform::MacOs).unwrap(),
                "\"macos\""
            );
            assert_eq!(
                serde_json::to_string(&HostPlatform::Linux).unwrap(),
                "\"linux\""
            );
        }

        #[test]
        fn deserializes_lowercase() {
            assert_eq!(
                serde_json::from_str::<HostPlatform>("\"macos\"").unwrap(),
                HostPlatform::MacOs
            );
            assert_eq!(
                serde_json::from_str::<HostPlatform>("\"linux\"").unwrap(),
                HostPlatform::Linux
            );
        }
    }
}
