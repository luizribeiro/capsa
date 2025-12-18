use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MountMode {
    #[default]
    ReadOnly,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ShareMechanism {
    // TODO: rename this as Default as it's actually going to use whatever the backend
    // considers to be the default?
    #[default]
    Auto,
    VirtioFs(VirtioFsConfig),
    Virtio9p(Virtio9pConfig),
}

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
