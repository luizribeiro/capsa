//! Sandbox configuration types.

use std::path::PathBuf;

/// Configuration for a Capsa sandbox VM.
///
/// The sandbox uses a capsa-controlled kernel and initrd that provides
/// guaranteed features like auto-mounting and a guest agent.
#[derive(Debug, Clone, Default)]
pub struct CapsaSandboxConfig {
    /// Override the default kernel (for testing/development).
    pub kernel_override: Option<PathBuf>,
    /// Override the default initrd (for testing/development).
    pub initrd_override: Option<PathBuf>,
}

impl CapsaSandboxConfig {
    /// Creates a new sandbox config with defaults.
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the default kernel path.
    pub fn with_kernel(mut self, path: impl Into<PathBuf>) -> Self {
        self.kernel_override = Some(path.into());
        self
    }

    /// Overrides the default initrd path.
    pub fn with_initrd(mut self, path: impl Into<PathBuf>) -> Self {
        self.initrd_override = Some(path.into());
        self
    }
}

/// What to run as the main process in the sandbox.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Will be used in build() implementation
pub enum MainProcess {
    /// Run a specific binary.
    Run {
        /// Path to the binary inside the guest.
        path: String,
        /// Arguments to pass to the binary.
        args: Vec<String>,
    },
    /// Run an OCI container.
    Oci {
        /// Container image reference (e.g., "python:3.11").
        image: String,
        /// Command and arguments to run in the container.
        args: Vec<String>,
    },
}

impl MainProcess {
    /// Creates a Run main process.
    pub fn run(path: impl Into<String>, args: &[&str]) -> Self {
        Self::Run {
            path: path.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }

    /// Creates an Oci main process.
    pub fn oci(image: impl Into<String>, args: &[&str]) -> Self {
        Self::Oci {
            image: image.into(),
            args: args.iter().map(|s| s.to_string()).collect(),
        }
    }
}

/// Configuration for a shared directory in the sandbox.
#[derive(Debug, Clone)]
#[allow(dead_code)] // Will be used in cmdline generation
pub struct ShareConfig {
    /// Host path to share.
    pub host_path: PathBuf,
    /// Guest path where the share will be mounted.
    pub guest_path: String,
    /// Whether the share is read-only.
    pub read_only: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    mod sandbox_config {
        use super::*;

        #[test]
        fn default_has_no_overrides() {
            let config = CapsaSandboxConfig::new();
            assert!(config.kernel_override.is_none());
            assert!(config.initrd_override.is_none());
        }

        #[test]
        fn with_kernel_sets_override() {
            let config = CapsaSandboxConfig::new().with_kernel("/custom/kernel");
            assert_eq!(
                config.kernel_override,
                Some(PathBuf::from("/custom/kernel"))
            );
        }

        #[test]
        fn with_initrd_sets_override() {
            let config = CapsaSandboxConfig::new().with_initrd("/custom/initrd");
            assert_eq!(
                config.initrd_override,
                Some(PathBuf::from("/custom/initrd"))
            );
        }
    }

    mod main_process {
        use super::*;

        #[test]
        fn run_creates_run_variant() {
            let mp = MainProcess::run("/bin/sh", &["-c", "echo hello"]);
            match mp {
                MainProcess::Run { path, args } => {
                    assert_eq!(path, "/bin/sh");
                    assert_eq!(args, vec!["-c", "echo hello"]);
                }
                _ => panic!("expected Run variant"),
            }
        }

        #[test]
        fn oci_creates_oci_variant() {
            let mp = MainProcess::oci("python:3.11", &["python", "script.py"]);
            match mp {
                MainProcess::Oci { image, args } => {
                    assert_eq!(image, "python:3.11");
                    assert_eq!(args, vec!["python", "script.py"]);
                }
                _ => panic!("expected Oci variant"),
            }
        }
    }
}
