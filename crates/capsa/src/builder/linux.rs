use crate::backend::select_backend;
use crate::handle::VmHandle;
use crate::pool::{No, Poolability, VmPool, Yes};
use capsa_core::{
    BackendCapabilities, DiskImage, Error, GuestOs, HypervisorBackend, ImageFormat, KernelCmdline,
    LinuxDirectBootConfig, MountMode, NetworkMode, ResourceConfig, Result, ShareMechanism,
    SharedDir, VmConfig,
};
use std::path::PathBuf;
use std::time::Duration;

/// Builder for configuring and creating Linux virtual machines.
///
/// Use [`Capsa::vm`](crate::Capsa::vm) for single VMs or
/// [`Capsa::pool`](crate::Capsa::pool) for VM pools.
///
/// ```rust,no_run
/// # use capsa::{Capsa, LinuxDirectBootConfig};
/// # async fn example() -> capsa::Result<()> {
/// let config = LinuxDirectBootConfig::new("./kernel", "./initrd");
/// let vm = Capsa::vm(config)
///     .cpus(2)
///     .memory_mb(1024)
///     .console_enabled()
///     .build().await?;
/// # Ok(())
/// # }
/// ```
///
/// See the [Getting Started guide](crate::guides::getting_started) for complete examples.
pub struct LinuxVmBuilder<P = No> {
    config: LinuxDirectBootConfig,
    resources: ResourceConfig,
    disks: Vec<DiskImage>,
    shares: Vec<SharedDir>,
    network: NetworkMode,
    console_enabled: bool,
    cmdline: KernelCmdline,
    #[allow(dead_code)]
    timeout: Option<Duration>,
    #[allow(dead_code)]
    poolable: Poolability<P>,
}

// TODO: break this down / organize further, as some of the properties and methods here would be
// helpful for other OSs as well that would be implemented in the future, also making this code
// simpler
// TODO: allow for backend type to be forced by caller, instead of automatically selecting it
// through select_backend

impl LinuxVmBuilder<No> {
    /// Creates a new builder for a single VM.
    pub(crate) fn new(config: LinuxDirectBootConfig) -> Self {
        Self {
            config,
            resources: ResourceConfig::default(),
            disks: Vec::new(),
            shares: Vec::new(),
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        }
    }

    /// Adds a disk to the VM (becomes /dev/vdb, /dev/vdc, etc.).
    pub fn disk(mut self, disk: impl Into<DiskImage>) -> Self {
        self.disks.push(disk.into());
        self
    }

    /// Builds and starts the virtual machine.
    ///
    /// Validates the configuration, selects an available backend,
    /// and starts the VM. Returns a handle for interacting with the running VM.
    pub async fn build(self) -> Result<VmHandle> {
        let backend = select_backend()?;
        self.validate(backend.capabilities())?;
        self.validate_disk_files()?;

        let cmdline = self.generate_cmdline(backend.as_ref());

        let internal_config = VmConfig {
            kernel: self.config.kernel,
            initrd: self.config.initrd,
            root_disk: self.config.root_disk,
            disks: self.disks,
            cmdline,
            resources: self.resources.clone(),
            shares: self.shares,
            network: self.network,
            console_enabled: self.console_enabled,
        };

        let backend_handle = backend.start(&internal_config).await?;

        Ok(VmHandle::new(
            backend_handle,
            GuestOs::Linux,
            self.resources,
        ))
    }
}

impl LinuxVmBuilder<Yes> {
    /// Creates a new builder for a VM pool.
    pub(crate) fn new_pool(config: LinuxDirectBootConfig) -> Self {
        Self {
            config,
            resources: ResourceConfig::default(),
            disks: Vec::new(),
            shares: Vec::new(),
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        }
    }

    /// Builds a pool of identical VMs for concurrent use.
    ///
    /// The pool pre-starts `size` VMs that can be acquired and released.
    pub async fn build(self, size: usize) -> Result<VmPool> {
        let backend = select_backend()?;
        self.validate(backend.capabilities())?;
        self.validate_disk_files()?;

        let cmdline = self.generate_cmdline(backend.as_ref());

        let internal_config = VmConfig {
            kernel: self.config.kernel,
            initrd: self.config.initrd,
            root_disk: self.config.root_disk,
            disks: self.disks,
            cmdline,
            resources: self.resources,
            shares: self.shares,
            network: self.network,
            console_enabled: self.console_enabled,
        };

        VmPool::new(internal_config, size).await
    }
}

impl<P> LinuxVmBuilder<P> {
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

    /// Sets a timeout for VM operations.
    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    /// Adds a shared directory between host and guest.
    ///
    /// The directory will be accessible inside the VM at the specified guest path.
    /// The sharing mechanism (virtio-fs or 9p) is automatically selected.
    pub fn share(
        mut self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
    ) -> Self {
        self.shares.push(SharedDir::new(host, guest, mode));
        self
    }

    /// Adds a shared directory with a specific sharing mechanism.
    pub fn share_with_mechanism(
        mut self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
        mechanism: ShareMechanism,
    ) -> Self {
        self.shares
            .push(SharedDir::with_mechanism(host, guest, mode, mechanism));
        self
    }

    /// Adds multiple shared directories.
    pub fn shares(mut self, shares: impl IntoIterator<Item = SharedDir>) -> Self {
        self.shares.extend(shares);
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

    /// Enables the console device for programmatic access via `vm.console()`.
    pub fn console_enabled(mut self) -> Self {
        self.console_enabled = true;
        self
    }

    /// Adds a kernel command line argument (key=value pair).
    pub fn cmdline_arg(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.cmdline.arg(key, value);
        self
    }

    /// Adds a kernel command line flag (single value without =).
    pub fn cmdline_flag(mut self, name: impl Into<String>) -> Self {
        self.cmdline.flag(name);
        self
    }

    /// Overrides the entire kernel command line with a custom string.
    ///
    /// This replaces all default arguments. Use with caution.
    pub fn cmdline_override(mut self, cmdline: impl Into<String>) -> Self {
        self.cmdline.override_with(cmdline);
        self
    }

    fn validate(&self, capabilities: &BackendCapabilities) -> Result<()> {
        if !capabilities.boot_methods.linux_direct {
            return Err(Error::UnsupportedFeature(
                "boot method: linux direct".into(),
            ));
        }

        if let Some(max) = capabilities.max_cpus {
            if self.resources.cpus > max {
                return Err(Error::InvalidConfig(format!(
                    "requested {} CPUs but backend supports at most {}",
                    self.resources.cpus, max
                )));
            }
        }

        if let Some(max) = capabilities.max_memory_mb {
            if self.resources.memory_mb > max {
                return Err(Error::InvalidConfig(format!(
                    "requested {} MB memory but backend supports at most {} MB",
                    self.resources.memory_mb, max
                )));
            }
        }

        match self.network {
            NetworkMode::None => {
                if !capabilities.network_modes.none {
                    return Err(Error::UnsupportedFeature("network mode: none".into()));
                }
            }
            NetworkMode::Nat => {
                if !capabilities.network_modes.nat {
                    return Err(Error::UnsupportedFeature("network mode: nat".into()));
                }
            }
        }

        // Validate all disks (root_disk and additional disks)
        let all_disks: Vec<&DiskImage> = self
            .config
            .root_disk
            .iter()
            .chain(self.disks.iter())
            .collect();

        for disk in all_disks {
            match disk.format {
                ImageFormat::Raw => {
                    if !capabilities.image_formats.raw {
                        return Err(Error::UnsupportedFeature("image format: raw".into()));
                    }
                }
                ImageFormat::Qcow2 => {
                    if !capabilities.image_formats.qcow2 {
                        return Err(Error::UnsupportedFeature("image format: qcow2".into()));
                    }
                }
            }
        }

        for share in &self.shares {
            match &share.mechanism {
                ShareMechanism::Auto => {}
                ShareMechanism::VirtioFs(_) => {
                    if !capabilities.share_mechanisms.virtio_fs {
                        return Err(Error::UnsupportedFeature("virtio-fs".into()));
                    }
                }
                ShareMechanism::Virtio9p(_) => {
                    if !capabilities.share_mechanisms.virtio_9p {
                        return Err(Error::UnsupportedFeature("virtio-9p".into()));
                    }
                }
            }
        }
        Ok(())
    }

    fn validate_disk_files(&self) -> Result<()> {
        let all_disks: Vec<&DiskImage> = self
            .config
            .root_disk
            .iter()
            .chain(self.disks.iter())
            .collect();

        for disk in all_disks {
            if disk.read_only {
                if !disk.path.exists() {
                    return Err(Error::InvalidConfig(format!(
                        "read-only disk not found: {}",
                        disk.path.display()
                    )));
                }
            } else {
                std::fs::OpenOptions::new()
                    .write(true)
                    .open(&disk.path)
                    .map_err(|e| {
                        Error::InvalidConfig(format!(
                            "disk not writable: {}: {}",
                            disk.path.display(),
                            e
                        ))
                    })?;
            }
        }
        Ok(())
    }

    // TODO: since this is only relevant to LinuxDirectBootConfig, we may possibly
    // move this whole logic into LinuxDirectBootConfig?
    fn generate_cmdline(&self, backend: &dyn HypervisorBackend) -> String {
        if self.cmdline.is_overridden() {
            return self.cmdline.build();
        }

        let mut cmdline = KernelCmdline::new();
        cmdline.merge(&backend.kernel_cmdline_defaults());

        // Only set root device if we have a root disk
        if self.config.root_disk.is_some() {
            cmdline.root(backend.default_root_device());
        }

        cmdline.merge(&self.cmdline);

        cmdline.build()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsa_core::{
        BackendCapabilities, BootMethodSupport, ImageFormatSupport, MountMode, NetworkModeSupport,
        ShareMechanismSupport, Virtio9pConfig, VirtioFsConfig,
    };
    use std::path::PathBuf;

    fn builder_with_network(network: NetworkMode) -> LinuxVmBuilder {
        LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: None,
            },
            resources: ResourceConfig::default(),
            disks: vec![],
            shares: vec![],
            network,
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        }
    }

    fn builder_with_resources(cpus: u32, memory_mb: u32) -> LinuxVmBuilder {
        LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: None,
            },
            resources: ResourceConfig { cpus, memory_mb },
            disks: vec![],
            shares: vec![],
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        }
    }

    fn builder_with_shares(shares: Vec<SharedDir>) -> LinuxVmBuilder {
        LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: None,
            },
            resources: ResourceConfig::default(),
            disks: vec![],
            shares,
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        }
    }

    fn builder_with_root_disk(format: ImageFormat) -> LinuxVmBuilder {
        LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: Some(DiskImage::with_format("/disk.img", format)),
            },
            resources: ResourceConfig::default(),
            disks: vec![],
            shares: vec![],
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        }
    }

    fn all_capabilities() -> BackendCapabilities {
        BackendCapabilities {
            boot_methods: BootMethodSupport { linux_direct: true },
            image_formats: ImageFormatSupport {
                raw: true,
                qcow2: true,
            },
            network_modes: NetworkModeSupport {
                none: true,
                nat: true,
            },
            share_mechanisms: ShareMechanismSupport {
                virtio_fs: true,
                virtio_9p: true,
            },
            ..Default::default()
        }
    }

    #[test]
    fn validate_linux_direct_boot_supported() {
        let builder = builder_with_shares(vec![]);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_linux_direct_boot_unsupported() {
        let builder = builder_with_shares(vec![]);
        let mut caps = all_capabilities();
        caps.boot_methods.linux_direct = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("linux direct")));
    }

    #[test]
    fn validate_no_shares() {
        let builder = builder_with_shares(vec![]);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_auto_mechanism_always_passes() {
        let builder =
            builder_with_shares(vec![SharedDir::new("/host", "/guest", MountMode::ReadOnly)]);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_virtio_fs_supported() {
        let builder = builder_with_shares(vec![SharedDir::with_mechanism(
            "/host",
            "/guest",
            MountMode::ReadOnly,
            ShareMechanism::VirtioFs(VirtioFsConfig::default()),
        )]);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_virtio_fs_unsupported() {
        let builder = builder_with_shares(vec![SharedDir::with_mechanism(
            "/host",
            "/guest",
            MountMode::ReadOnly,
            ShareMechanism::VirtioFs(VirtioFsConfig::default()),
        )]);
        let mut caps = all_capabilities();
        caps.share_mechanisms.virtio_fs = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f == "virtio-fs"));
    }

    #[test]
    fn validate_virtio_9p_supported() {
        let builder = builder_with_shares(vec![SharedDir::with_mechanism(
            "/host",
            "/guest",
            MountMode::ReadOnly,
            ShareMechanism::Virtio9p(Virtio9pConfig::default()),
        )]);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_virtio_9p_unsupported() {
        let builder = builder_with_shares(vec![SharedDir::with_mechanism(
            "/host",
            "/guest",
            MountMode::ReadOnly,
            ShareMechanism::Virtio9p(Virtio9pConfig::default()),
        )]);
        let mut caps = all_capabilities();
        caps.share_mechanisms.virtio_9p = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f == "virtio-9p"));
    }

    #[test]
    fn validate_cpus_within_limit() {
        let builder = builder_with_resources(4, 1024);
        let mut caps = all_capabilities();
        caps.max_cpus = Some(8);
        assert!(builder.validate(&caps).is_ok());
    }

    #[test]
    fn validate_cpus_exceeds_limit() {
        let builder = builder_with_resources(16, 1024);
        let mut caps = all_capabilities();
        caps.max_cpus = Some(8);
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("16 CPUs")));
    }

    #[test]
    fn validate_cpus_no_limit() {
        let builder = builder_with_resources(128, 1024);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_memory_within_limit() {
        let builder = builder_with_resources(1, 4096);
        let mut caps = all_capabilities();
        caps.max_memory_mb = Some(8192);
        assert!(builder.validate(&caps).is_ok());
    }

    #[test]
    fn validate_memory_exceeds_limit() {
        let builder = builder_with_resources(1, 16384);
        let mut caps = all_capabilities();
        caps.max_memory_mb = Some(8192);
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("16384 MB")));
    }

    #[test]
    fn validate_memory_no_limit() {
        let builder = builder_with_resources(1, 65536);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_network_none_supported() {
        let builder = builder_with_network(NetworkMode::None);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_network_none_unsupported() {
        let builder = builder_with_network(NetworkMode::None);
        let mut caps = all_capabilities();
        caps.network_modes.none = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("none")));
    }

    #[test]
    fn validate_network_nat_supported() {
        let builder = builder_with_network(NetworkMode::Nat);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_network_nat_unsupported() {
        let builder = builder_with_network(NetworkMode::Nat);
        let mut caps = all_capabilities();
        caps.network_modes.nat = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("nat")));
    }

    #[test]
    fn validate_no_disk() {
        let builder = builder_with_shares(vec![]);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_raw_disk_supported() {
        let builder = builder_with_root_disk(ImageFormat::Raw);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_raw_disk_unsupported() {
        let builder = builder_with_root_disk(ImageFormat::Raw);
        let mut caps = all_capabilities();
        caps.image_formats.raw = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("raw")));
    }

    #[test]
    fn validate_qcow2_disk_supported() {
        let builder = builder_with_root_disk(ImageFormat::Qcow2);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_qcow2_disk_unsupported() {
        let builder = builder_with_root_disk(ImageFormat::Qcow2);
        let mut caps = all_capabilities();
        caps.image_formats.qcow2 = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("qcow2")));
    }

    #[test]
    fn validate_disk_files_readonly_exists() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        let disk = DiskImage::new(temp_file.path()).read_only();
        let builder: LinuxVmBuilder<Yes> = LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: Some(disk),
            },
            resources: ResourceConfig::default(),
            disks: vec![],
            shares: vec![],
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        };
        assert!(builder.validate_disk_files().is_ok());
    }

    #[test]
    fn validate_disk_files_readonly_not_found() {
        let disk = DiskImage::new("/nonexistent/path/disk.raw").read_only();
        let builder: LinuxVmBuilder<Yes> = LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: Some(disk),
            },
            resources: ResourceConfig::default(),
            disks: vec![],
            shares: vec![],
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        };
        let err = builder.validate_disk_files().unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("not found")));
    }

    #[test]
    fn validate_disk_files_writable() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        let disk = DiskImage::new(temp_file.path());
        let builder: LinuxVmBuilder<Yes> = LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: Some(disk),
            },
            resources: ResourceConfig::default(),
            disks: vec![],
            shares: vec![],
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        };
        assert!(builder.validate_disk_files().is_ok());
    }

    #[test]
    fn validate_disk_files_not_writable() {
        let disk = DiskImage::new("/nonexistent/path/disk.raw");
        let builder: LinuxVmBuilder<Yes> = LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: Some(disk),
            },
            resources: ResourceConfig::default(),
            disks: vec![],
            shares: vec![],
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        };
        let err = builder.validate_disk_files().unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("not writable")));
    }

    #[test]
    fn disk_method_adds_to_disks_vector() {
        let config = LinuxDirectBootConfig::new("/kernel", "/initrd");
        let builder = LinuxVmBuilder::new(config)
            .disk(DiskImage::new("/disk1.raw"))
            .disk(DiskImage::new("/disk2.raw"));

        assert!(builder.config.root_disk.is_none());
        assert_eq!(builder.disks.len(), 2);
        assert_eq!(builder.disks[0].path, PathBuf::from("/disk1.raw"));
        assert_eq!(builder.disks[1].path, PathBuf::from("/disk2.raw"));
    }

    #[test]
    fn root_disk_and_additional_disks_separate() {
        let config = LinuxDirectBootConfig::new("/kernel", "/initrd")
            .with_root_disk(DiskImage::new("/root.raw"));
        let builder = LinuxVmBuilder::new(config)
            .disk(DiskImage::new("/data1.raw"))
            .disk(DiskImage::new("/data2.raw"));

        assert!(builder.config.root_disk.is_some());
        assert_eq!(
            builder.config.root_disk.as_ref().unwrap().path,
            PathBuf::from("/root.raw")
        );
        assert_eq!(builder.disks.len(), 2);
    }

    #[test]
    fn validate_additional_disk_format_unsupported() {
        let config = LinuxDirectBootConfig::new("/kernel", "/initrd");
        let builder = LinuxVmBuilder::new(config)
            .disk(DiskImage::with_format("/disk.qcow2", ImageFormat::Qcow2));

        let mut caps = all_capabilities();
        caps.image_formats.qcow2 = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("qcow2")));
    }

    #[test]
    fn validate_disk_files_additional_disk_readonly_not_found() {
        let disk = DiskImage::new("/nonexistent/additional.raw").read_only();
        let builder: LinuxVmBuilder<Yes> = LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: None,
            },
            resources: ResourceConfig::default(),
            disks: vec![disk],
            shares: vec![],
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        };
        let err = builder.validate_disk_files().unwrap_err();
        assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("not found")));
    }

    #[test]
    fn validate_disk_files_additional_disk_writable() {
        let temp_file = tempfile::NamedTempFile::new().unwrap();
        let disk = DiskImage::new(temp_file.path());
        let builder: LinuxVmBuilder<Yes> = LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: None,
            },
            resources: ResourceConfig::default(),
            disks: vec![disk],
            shares: vec![],
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        };
        assert!(builder.validate_disk_files().is_ok());
    }

    #[test]
    fn validate_disk_files_mixed_root_and_additional() {
        let temp_root = tempfile::NamedTempFile::new().unwrap();
        let temp_additional = tempfile::NamedTempFile::new().unwrap();

        let builder: LinuxVmBuilder<Yes> = LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                root_disk: Some(DiskImage::new(temp_root.path()).read_only()),
            },
            resources: ResourceConfig::default(),
            disks: vec![DiskImage::new(temp_additional.path())],
            shares: vec![],
            network: NetworkMode::default(),
            console_enabled: false,
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        };
        assert!(builder.validate_disk_files().is_ok());
    }
}
