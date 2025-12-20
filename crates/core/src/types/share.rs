use serde::{Deserialize, Serialize};
use std::path::PathBuf;

/// Access mode for shared directories.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    /// Guest can read but not modify files.
    #[default]
    ReadOnly,
    /// Guest can read and write files.
    ReadWrite,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct VirtioFsConfig {
    pub tag: Option<String>,
    pub cache: Option<String>,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Virtio9pConfig {
    pub tag: Option<String>,
    pub msize: Option<u32>,
}

/// Mechanism for sharing directories with the guest.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ShareMechanism {
    /// Automatically select the best available mechanism.
    #[default]
    Auto,
    /// Use virtio-fs (higher performance).
    VirtioFs(VirtioFsConfig),
    /// Use virtio-9p (wider compatibility).
    Virtio9p(Virtio9pConfig),
}

impl ShareMechanism {
    /// Use virtio-fs with default configuration.
    pub fn virtio_fs() -> Self {
        Self::VirtioFs(VirtioFsConfig::default())
    }

    /// Use virtio-9p with default configuration.
    pub fn virtio_9p() -> Self {
        Self::Virtio9p(Virtio9pConfig::default())
    }
}

/// A directory shared between host and guest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedDir {
    pub host_path: PathBuf,
    pub guest_path: String,
    pub mode: MountMode,
    #[serde(default)]
    pub mechanism: ShareMechanism,
}

impl SharedDir {
    pub fn new(
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
        mode: MountMode,
    ) -> Self {
        Self {
            host_path: host_path.into(),
            guest_path: guest_path.into(),
            mode,
            mechanism: ShareMechanism::default(),
        }
    }

    pub fn with_mechanism(
        host_path: impl Into<PathBuf>,
        guest_path: impl Into<String>,
        mode: MountMode,
        mechanism: ShareMechanism,
    ) -> Self {
        Self {
            host_path: host_path.into(),
            guest_path: guest_path.into(),
            mode,
            mechanism,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod mount_mode {
        use super::*;

        #[test]
        fn default_is_read_only() {
            assert_eq!(MountMode::default(), MountMode::ReadOnly);
        }

        #[test]
        fn serializes_lowercase() {
            assert_eq!(
                serde_json::to_string(&MountMode::ReadOnly).unwrap(),
                "\"readonly\""
            );
            assert_eq!(
                serde_json::to_string(&MountMode::ReadWrite).unwrap(),
                "\"readwrite\""
            );
        }
    }

    mod share_mechanism {
        use super::*;

        #[test]
        fn default_is_auto() {
            assert!(matches!(ShareMechanism::default(), ShareMechanism::Auto));
        }

        #[test]
        fn virtio_fs_serializes_with_tag() {
            let mechanism = ShareMechanism::VirtioFs(VirtioFsConfig {
                tag: Some("share0".to_string()),
                cache: Some("auto".to_string()),
            });
            let json = serde_json::to_string(&mechanism).unwrap();
            assert!(json.contains("\"type\":\"virtiofs\""));
            assert!(json.contains("\"tag\":\"share0\""));
        }

        #[test]
        fn virtio_9p_serializes_with_tag() {
            let mechanism = ShareMechanism::Virtio9p(Virtio9pConfig {
                tag: Some("share0".to_string()),
                msize: Some(8192),
            });
            let json = serde_json::to_string(&mechanism).unwrap();
            assert!(json.contains("\"type\":\"virtio9p\""));
            assert!(json.contains("\"tag\":\"share0\""));
        }
    }

    mod shared_dir {
        use super::*;

        #[test]
        fn new_creates_with_auto_mechanism() {
            let share = SharedDir::new("/host/path", "/guest/path", MountMode::ReadWrite);
            assert_eq!(share.host_path, PathBuf::from("/host/path"));
            assert_eq!(share.guest_path, "/guest/path");
            assert_eq!(share.mode, MountMode::ReadWrite);
            assert!(matches!(share.mechanism, ShareMechanism::Auto));
        }

        #[test]
        fn with_mechanism_sets_mechanism() {
            let share = SharedDir::with_mechanism(
                "/host/path",
                "/guest/path",
                MountMode::ReadOnly,
                ShareMechanism::VirtioFs(VirtioFsConfig::default()),
            );
            assert!(matches!(share.mechanism, ShareMechanism::VirtioFs(_)));
        }

        #[test]
        fn serialization_roundtrip() {
            let share = SharedDir::new("/host/path", "/guest/path", MountMode::ReadWrite);
            let json = serde_json::to_string(&share).unwrap();
            let deserialized: SharedDir = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.host_path, share.host_path);
            assert_eq!(deserialized.guest_path, share.guest_path);
            assert_eq!(deserialized.mode, share.mode);
        }
    }
}
