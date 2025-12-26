//! Sandbox builder with typestate pattern.

use super::config::{CapsaSandboxConfig, MainProcess, ShareConfig};
use capsa_core::{MountMode, NetworkMode, ResourceConfig, VsockConfig};
use std::marker::PhantomData;
use std::path::PathBuf;

/// Marker: no main process specified yet.
pub struct NoMainProcess;

/// Marker: main process has been specified.
pub struct HasMainProcess;

/// Builder for sandbox VMs with typestate for main process.
///
/// The typestate pattern enforces at compile time that:
/// - `.run()` and `.oci()` are mutually exclusive
/// - `.build()` is only available after specifying a main process
///
/// # Example
///
/// ```rust,ignore
/// let vm = Capsa::sandbox()
///     .share("./workspace", "/mnt", MountMode::ReadWrite)
///     .run("/bin/sh", &[])
///     .build()
///     .await?;
/// ```
pub struct SandboxBuilder<M = NoMainProcess> {
    pub(crate) config: CapsaSandboxConfig,
    pub(crate) shares: Vec<ShareConfig>,
    #[allow(dead_code)] // Will be used in build() implementation
    pub(crate) main_process: Option<MainProcess>,
    pub(crate) resources: ResourceConfig,
    pub(crate) network: NetworkMode,
    pub(crate) console_enabled: bool,
    pub(crate) vsock: VsockConfig,
    pub(crate) _marker: PhantomData<M>,
}

impl SandboxBuilder<NoMainProcess> {
    /// Creates a new sandbox builder.
    pub fn new() -> Self {
        Self {
            config: CapsaSandboxConfig::new(),
            shares: Vec::new(),
            main_process: None,
            resources: ResourceConfig::default(),
            network: NetworkMode::default(),
            console_enabled: true,
            vsock: VsockConfig::default(),
            _marker: PhantomData,
        }
    }

    /// Run a binary as the main process.
    ///
    /// Can only be called once, and cannot be combined with `.oci()`.
    pub fn run(self, path: impl Into<String>, args: &[&str]) -> SandboxBuilder<HasMainProcess> {
        SandboxBuilder {
            config: self.config,
            shares: self.shares,
            main_process: Some(MainProcess::run(path, args)),
            resources: self.resources,
            network: self.network,
            console_enabled: self.console_enabled,
            vsock: self.vsock,
            _marker: PhantomData,
        }
    }

    /// Run an OCI container as the main process.
    ///
    /// Can only be called once, and cannot be combined with `.run()`.
    pub fn oci(self, image: impl Into<String>, args: &[&str]) -> SandboxBuilder<HasMainProcess> {
        SandboxBuilder {
            config: self.config,
            shares: self.shares,
            main_process: Some(MainProcess::oci(image, args)),
            resources: self.resources,
            network: self.network,
            console_enabled: self.console_enabled,
            vsock: self.vsock,
            _marker: PhantomData,
        }
    }
}

impl Default for SandboxBuilder<NoMainProcess> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> SandboxBuilder<M> {
    /// Share a directory with automatic mounting.
    ///
    /// The directory will be mounted at the specified guest path when the
    /// sandbox boots.
    pub fn share(
        mut self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
    ) -> Self {
        self.shares.push(ShareConfig {
            host_path: host.into(),
            guest_path: guest.into(),
            read_only: mode == MountMode::ReadOnly,
        });
        self
    }

    /// Sets the number of virtual CPUs for the VM.
    pub fn cpus(mut self, count: u32) -> Self {
        self.resources.cpus = count;
        self
    }

    /// Sets the amount of memory in megabytes for the VM.
    pub fn memory_mb(mut self, mb: u32) -> Self {
        self.resources.memory_mb = mb;
        self
    }

    /// Sets the network mode for the VM.
    pub fn network(mut self, mode: NetworkMode) -> Self {
        self.network = mode;
        self
    }

    /// Disables networking for the VM.
    pub fn no_network(self) -> Self {
        self.network(NetworkMode::None)
    }

    /// Overrides the default sandbox kernel.
    pub fn kernel(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.kernel_override = Some(path.into());
        self
    }

    /// Overrides the default sandbox initrd.
    pub fn initrd(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.initrd_override = Some(path.into());
        self
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    mod typestate {
        use super::*;

        #[test]
        fn new_builder_has_no_main_process() {
            let builder = SandboxBuilder::new();
            assert!(builder.main_process.is_none());
        }

        #[test]
        fn run_sets_main_process() {
            let builder = SandboxBuilder::new().run("/bin/sh", &["-c", "echo hi"]);
            assert!(builder.main_process.is_some());
            match builder.main_process.as_ref().unwrap() {
                MainProcess::Run { path, args } => {
                    assert_eq!(path, "/bin/sh");
                    assert_eq!(args, &["-c", "echo hi"]);
                }
                _ => panic!("expected Run"),
            }
        }

        #[test]
        fn oci_sets_main_process() {
            let builder = SandboxBuilder::new().oci("python:3.11", &["python"]);
            assert!(builder.main_process.is_some());
            match builder.main_process.as_ref().unwrap() {
                MainProcess::Oci { image, args } => {
                    assert_eq!(image, "python:3.11");
                    assert_eq!(args, &["python"]);
                }
                _ => panic!("expected Oci"),
            }
        }
    }

    mod builder_methods {
        use super::*;

        #[test]
        fn share_adds_to_shares() {
            let builder = SandboxBuilder::new()
                .share("./src", "/mnt/src", MountMode::ReadOnly)
                .share("./data", "/mnt/data", MountMode::ReadWrite);

            assert_eq!(builder.shares.len(), 2);
            assert_eq!(builder.shares[0].host_path, PathBuf::from("./src"));
            assert_eq!(builder.shares[0].guest_path, "/mnt/src");
            assert!(builder.shares[0].read_only);
            assert_eq!(builder.shares[1].host_path, PathBuf::from("./data"));
            assert!(!builder.shares[1].read_only);
        }

        #[test]
        fn share_works_before_run() {
            let builder = SandboxBuilder::new()
                .share("./src", "/mnt", MountMode::ReadOnly)
                .run("/bin/sh", &[]);

            assert_eq!(builder.shares.len(), 1);
            assert!(builder.main_process.is_some());
        }

        #[test]
        fn share_works_after_run() {
            let builder = SandboxBuilder::new().run("/bin/sh", &[]).share(
                "./src",
                "/mnt",
                MountMode::ReadOnly,
            );

            assert_eq!(builder.shares.len(), 1);
            assert!(builder.main_process.is_some());
        }

        #[test]
        fn cpus_sets_value() {
            let builder = SandboxBuilder::new().cpus(4);
            assert_eq!(builder.resources.cpus, 4);
        }

        #[test]
        fn memory_mb_sets_value() {
            let builder = SandboxBuilder::new().memory_mb(2048);
            assert_eq!(builder.resources.memory_mb, 2048);
        }

        #[test]
        fn no_network_sets_none() {
            let builder = SandboxBuilder::new().no_network();
            assert!(matches!(builder.network, NetworkMode::None));
        }

        #[test]
        fn kernel_override() {
            let builder = SandboxBuilder::new().kernel("/custom/kernel");
            assert_eq!(
                builder.config.kernel_override,
                Some(PathBuf::from("/custom/kernel"))
            );
        }

        #[test]
        fn initrd_override() {
            let builder = SandboxBuilder::new().initrd("/custom/initrd");
            assert_eq!(
                builder.config.initrd_override,
                Some(PathBuf::from("/custom/initrd"))
            );
        }

        #[test]
        fn console_enabled_by_default() {
            let builder = SandboxBuilder::new();
            assert!(builder.console_enabled);
        }
    }
}
