use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImageFormat {
    #[default]
    Raw,
    Qcow2,
}

impl ImageFormat {
    pub fn from_extension(path: &Path) -> Option<Self> {
        let ext = path.extension()?.to_str()?;
        match ext.to_lowercase().as_str() {
            "raw" | "img" => Some(Self::Raw),
            "qcow2" => Some(Self::Qcow2),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskImage {
    pub path: PathBuf,
    pub format: ImageFormat,
}

impl DiskImage {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let format = ImageFormat::from_extension(&path).unwrap_or_default();
        Self { path, format }
    }

    pub fn with_format(path: impl Into<PathBuf>, format: ImageFormat) -> Self {
        Self {
            path: path.into(),
            format,
        }
    }
}
