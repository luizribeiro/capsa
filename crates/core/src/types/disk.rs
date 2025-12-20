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

/// A disk image to attach to a VM.
///
/// Format is inferred from file extension (`.raw`, `.img`, `.qcow2`).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiskImage {
    pub path: PathBuf,
    pub format: ImageFormat,
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_only: bool,
}

impl<T: Into<PathBuf>> From<T> for DiskImage {
    fn from(path: T) -> Self {
        Self::new(path)
    }
}

impl DiskImage {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        let path = path.into();
        let format = ImageFormat::from_extension(&path).unwrap_or_default();
        Self {
            path,
            format,
            read_only: false,
        }
    }

    pub fn with_format(path: impl Into<PathBuf>, format: ImageFormat) -> Self {
        Self {
            path: path.into(),
            format,
            read_only: false,
        }
    }

    pub fn read_only(mut self) -> Self {
        self.read_only = true;
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod image_format {
        use super::*;

        #[test]
        fn from_extension_raw() {
            assert_eq!(
                ImageFormat::from_extension(Path::new("disk.raw")),
                Some(ImageFormat::Raw)
            );
        }

        #[test]
        fn from_extension_img() {
            assert_eq!(
                ImageFormat::from_extension(Path::new("disk.img")),
                Some(ImageFormat::Raw)
            );
        }

        #[test]
        fn from_extension_qcow2() {
            assert_eq!(
                ImageFormat::from_extension(Path::new("disk.qcow2")),
                Some(ImageFormat::Qcow2)
            );
        }

        #[test]
        fn from_extension_unknown() {
            assert_eq!(ImageFormat::from_extension(Path::new("disk.vmdk")), None);
        }

        #[test]
        fn from_extension_no_extension() {
            assert_eq!(ImageFormat::from_extension(Path::new("disk")), None);
        }

        #[test]
        fn from_extension_case_insensitive() {
            assert_eq!(
                ImageFormat::from_extension(Path::new("disk.RAW")),
                Some(ImageFormat::Raw)
            );
            assert_eq!(
                ImageFormat::from_extension(Path::new("disk.QCOW2")),
                Some(ImageFormat::Qcow2)
            );
        }

        #[test]
        fn default_is_raw() {
            assert_eq!(ImageFormat::default(), ImageFormat::Raw);
        }
    }

    mod disk_image {
        use super::*;

        #[test]
        fn new_infers_raw_format() {
            let disk = DiskImage::new("/path/to/disk.raw");
            assert_eq!(disk.path, PathBuf::from("/path/to/disk.raw"));
            assert_eq!(disk.format, ImageFormat::Raw);
            assert!(!disk.read_only);
        }

        #[test]
        fn new_infers_qcow2_format() {
            let disk = DiskImage::new("/path/to/disk.qcow2");
            assert_eq!(disk.format, ImageFormat::Qcow2);
            assert!(!disk.read_only);
        }

        #[test]
        fn new_defaults_to_raw_for_unknown() {
            let disk = DiskImage::new("/path/to/disk.unknown");
            assert_eq!(disk.format, ImageFormat::Raw);
        }

        #[test]
        fn with_format_overrides_inference() {
            let disk = DiskImage::with_format("/path/to/disk.raw", ImageFormat::Qcow2);
            assert_eq!(disk.path, PathBuf::from("/path/to/disk.raw"));
            assert_eq!(disk.format, ImageFormat::Qcow2);
            assert!(!disk.read_only);
        }

        #[test]
        fn read_only_sets_flag() {
            let disk = DiskImage::new("/path/to/disk.raw").read_only();
            assert!(disk.read_only);
        }

        #[test]
        fn read_only_chains_with_format() {
            let disk = DiskImage::with_format("/path/to/disk.raw", ImageFormat::Qcow2).read_only();
            assert_eq!(disk.format, ImageFormat::Qcow2);
            assert!(disk.read_only);
        }
    }

    mod serialization {
        use super::*;

        #[test]
        fn image_format_serializes_lowercase() {
            assert_eq!(serde_json::to_string(&ImageFormat::Raw).unwrap(), "\"raw\"");
            assert_eq!(
                serde_json::to_string(&ImageFormat::Qcow2).unwrap(),
                "\"qcow2\""
            );
        }

        #[test]
        fn image_format_deserializes_lowercase() {
            assert_eq!(
                serde_json::from_str::<ImageFormat>("\"raw\"").unwrap(),
                ImageFormat::Raw
            );
            assert_eq!(
                serde_json::from_str::<ImageFormat>("\"qcow2\"").unwrap(),
                ImageFormat::Qcow2
            );
        }

        #[test]
        fn disk_image_roundtrip() {
            let disk = DiskImage::new("/path/to/disk.qcow2");
            let json = serde_json::to_string(&disk).unwrap();
            let deserialized: DiskImage = serde_json::from_str(&json).unwrap();
            assert_eq!(deserialized.path, disk.path);
            assert_eq!(deserialized.format, disk.format);
            assert_eq!(deserialized.read_only, disk.read_only);
        }

        #[test]
        fn disk_image_omits_read_only_when_false() {
            let disk = DiskImage::new("/path/to/disk.raw");
            let json = serde_json::to_string(&disk).unwrap();
            assert!(!json.contains("read_only"));
        }

        #[test]
        fn disk_image_includes_read_only_when_true() {
            let disk = DiskImage::new("/path/to/disk.raw").read_only();
            let json = serde_json::to_string(&disk).unwrap();
            assert!(json.contains("read_only"));
            assert!(json.contains("true"));
        }

        #[test]
        fn disk_image_read_only_roundtrip() {
            let disk = DiskImage::new("/path/to/disk.qcow2").read_only();
            let json = serde_json::to_string(&disk).unwrap();
            let deserialized: DiskImage = serde_json::from_str(&json).unwrap();
            assert!(deserialized.read_only);
        }
    }
}
