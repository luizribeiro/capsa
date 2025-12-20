use crate::backend::select_backend;
use crate::handle::VmHandle;
use crate::pool::{No, Poolability, VmPool, Yes};
use capsa_core::{
    BackendCapabilities, DiskImage, Error, GuestOs, HypervisorBackend, ImageFormat, MountMode,
    NetworkMode, ResourceConfig, Result, ShareMechanism, SharedDir, VmConfig,
};
use std::path::PathBuf;
use std::time::Duration;

/// Trait for boot configurations that can be used with `VmBuilder`.
///
/// This trait defines the boot-specific behavior needed to build and validate VMs.
pub trait BootConfigBuilder: Clone {
    /// Validates boot-specific capabilities.
    fn validate_boot(&self, capabilities: &BackendCapabilities) -> Result<()>;

    /// Validates boot-specific disk files exist and are accessible.
    fn validate_boot_disk_files(&self) -> Result<()>;

    /// Returns the boot disk for capability validation, if any.
    fn boot_disk(&self) -> Option<&DiskImage>;

    /// Converts this config into a VmConfig with the given common settings.
    ///
    /// Returns the VmConfig and an optional path to a temp file that should be
    /// cleaned up when the VM stops (e.g., auto-generated EFI variable store).
    fn into_vm_config(
        self,
        disks: Vec<DiskImage>,
        resources: ResourceConfig,
        shares: Vec<SharedDir>,
        network: NetworkMode,
        console_enabled: bool,
        backend: &dyn HypervisorBackend,
    ) -> (VmConfig, Option<PathBuf>);
}

/// Builder for configuring and creating virtual machines.
///
/// Use [`Capsa::vm`](crate::Capsa::vm) for single VMs or
/// [`Capsa::pool`](crate::Capsa::pool) for VM pools.
///
/// # Example
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
pub struct VmBuilder<B: BootConfigBuilder, P = No> {
    pub(crate) boot_config: B,
    pub(crate) resources: ResourceConfig,
    pub(crate) disks: Vec<DiskImage>,
    pub(crate) shares: Vec<SharedDir>,
    pub(crate) network: NetworkMode,
    pub(crate) console_enabled: bool,
    #[allow(dead_code)]
    pub(crate) timeout: Option<Duration>,
    #[allow(dead_code)]
    pub(crate) poolable: Poolability<P>,
}

// TODO: Allow backend type to be forced by caller, instead of automatically selecting it
// through select_backend()
impl<B: BootConfigBuilder> VmBuilder<B, No> {
    /// Creates a new builder for a single VM.
    pub fn new(boot_config: B) -> Self {
        Self {
            boot_config,
            resources: ResourceConfig::default(),
            disks: Vec::new(),
            shares: Vec::new(),
            network: NetworkMode::default(),
            console_enabled: false,
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
    pub async fn build(self) -> Result<VmHandle> {
        let backend = select_backend()?;
        self.validate(backend.capabilities())?;
        self.validate_disk_files()?;

        let (internal_config, temp_file) = self.boot_config.into_vm_config(
            self.disks,
            self.resources.clone(),
            self.shares,
            self.network,
            self.console_enabled,
            backend.as_ref(),
        );

        let backend_handle = backend.start(&internal_config).await?;

        let mut handle = VmHandle::new(backend_handle, GuestOs::Linux, self.resources);
        if let Some(path) = temp_file {
            handle = handle.with_temp_file(path);
        }
        Ok(handle)
    }
}

impl<B: BootConfigBuilder> VmBuilder<B, Yes> {
    /// Creates a new builder for a VM pool.
    pub fn new_pool(boot_config: B) -> Self {
        Self {
            boot_config,
            resources: ResourceConfig::default(),
            disks: Vec::new(),
            shares: Vec::new(),
            network: NetworkMode::default(),
            console_enabled: false,
            timeout: None,
            poolable: Poolability::new(),
        }
    }

    /// Builds a pool of identical VMs for concurrent use.
    pub async fn build(self, size: usize) -> Result<VmPool> {
        let backend = select_backend()?;
        self.validate(backend.capabilities())?;
        self.validate_disk_files()?;

        // For pools, temp files are managed per-VM instance by VmPool::spawn_vm
        let (internal_config, _) = self.boot_config.into_vm_config(
            self.disks,
            self.resources,
            self.shares,
            self.network,
            self.console_enabled,
            backend.as_ref(),
        );

        VmPool::new(internal_config, size).await
    }
}

impl<B: BootConfigBuilder, P> VmBuilder<B, P> {
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

    fn validate(&self, capabilities: &BackendCapabilities) -> Result<()> {
        self.boot_config.validate_boot(capabilities)?;

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

        // Validate boot disk format if present
        if let Some(boot_disk) = self.boot_config.boot_disk() {
            Self::validate_disk_format(boot_disk, capabilities)?;
        }

        // Validate additional disks
        for disk in &self.disks {
            Self::validate_disk_format(disk, capabilities)?;
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

    fn validate_disk_format(disk: &DiskImage, capabilities: &BackendCapabilities) -> Result<()> {
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
        Ok(())
    }

    fn validate_disk_files(&self) -> Result<()> {
        self.boot_config.validate_boot_disk_files()?;

        for disk in &self.disks {
            Self::validate_single_disk_file(disk)?;
        }

        Ok(())
    }

    fn validate_single_disk_file(disk: &DiskImage) -> Result<()> {
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
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use capsa_core::{
        BackendCapabilities, BootMethodSupport, ImageFormatSupport, LinuxDirectBootConfig,
        MountMode, NetworkModeSupport, ShareMechanismSupport, UefiBootConfig, Virtio9pConfig,
        VirtioFsConfig,
    };
    use std::path::PathBuf;

    fn linux_builder() -> VmBuilder<LinuxDirectBootConfig> {
        VmBuilder::new(LinuxDirectBootConfig::new("/kernel", "/initrd"))
    }

    fn linux_builder_with_root_disk(format: ImageFormat) -> VmBuilder<LinuxDirectBootConfig> {
        let config = LinuxDirectBootConfig::new("/kernel", "/initrd")
            .with_root_disk(DiskImage::with_format("/disk.img", format));
        VmBuilder::new(config)
    }

    fn uefi_builder() -> VmBuilder<UefiBootConfig> {
        VmBuilder::new(UefiBootConfig::new("/disk.raw"))
    }

    fn all_capabilities() -> BackendCapabilities {
        BackendCapabilities {
            boot_methods: BootMethodSupport {
                linux_direct: true,
                uefi: true,
            },
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

    mod linux_boot_validation {
        use super::*;

        #[test]
        fn linux_direct_boot_supported() {
            let builder = linux_builder();
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn linux_direct_boot_unsupported() {
            let builder = linux_builder();
            let mut caps = all_capabilities();
            caps.boot_methods.linux_direct = false;
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("linux direct")));
        }
    }

    mod uefi_boot_validation {
        use super::*;

        #[test]
        fn uefi_boot_supported() {
            let builder = uefi_builder();
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn uefi_boot_unsupported() {
            let builder = uefi_builder();
            let mut caps = all_capabilities();
            caps.boot_methods.uefi = false;
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("uefi")));
        }
    }

    mod resource_validation {
        use super::*;

        #[test]
        fn cpus_within_limit() {
            let builder = linux_builder().cpus(4);
            let mut caps = all_capabilities();
            caps.max_cpus = Some(8);
            assert!(builder.validate(&caps).is_ok());
        }

        #[test]
        fn cpus_exceeds_limit() {
            let builder = linux_builder().cpus(16);
            let mut caps = all_capabilities();
            caps.max_cpus = Some(8);
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("16 CPUs")));
        }

        #[test]
        fn cpus_no_limit() {
            let builder = linux_builder().cpus(128);
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn memory_within_limit() {
            let builder = linux_builder().memory_mb(4096);
            let mut caps = all_capabilities();
            caps.max_memory_mb = Some(8192);
            assert!(builder.validate(&caps).is_ok());
        }

        #[test]
        fn memory_exceeds_limit() {
            let builder = linux_builder().memory_mb(16384);
            let mut caps = all_capabilities();
            caps.max_memory_mb = Some(8192);
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("16384 MB")));
        }

        #[test]
        fn memory_no_limit() {
            let builder = linux_builder().memory_mb(65536);
            assert!(builder.validate(&all_capabilities()).is_ok());
        }
    }

    mod network_validation {
        use super::*;

        #[test]
        fn none_supported() {
            let builder = linux_builder().no_network();
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn none_unsupported() {
            let builder = linux_builder().no_network();
            let mut caps = all_capabilities();
            caps.network_modes.none = false;
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("none")));
        }

        #[test]
        fn nat_supported() {
            let builder = linux_builder().network(NetworkMode::Nat);
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn nat_unsupported() {
            let builder = linux_builder().network(NetworkMode::Nat);
            let mut caps = all_capabilities();
            caps.network_modes.nat = false;
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("nat")));
        }
    }

    mod disk_format_validation {
        use super::*;

        #[test]
        fn no_disk() {
            let builder = linux_builder();
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn raw_disk_supported() {
            let builder = linux_builder_with_root_disk(ImageFormat::Raw);
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn raw_disk_unsupported() {
            let builder = linux_builder_with_root_disk(ImageFormat::Raw);
            let mut caps = all_capabilities();
            caps.image_formats.raw = false;
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("raw")));
        }

        #[test]
        fn qcow2_disk_supported() {
            let builder = linux_builder_with_root_disk(ImageFormat::Qcow2);
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn qcow2_disk_unsupported() {
            let builder = linux_builder_with_root_disk(ImageFormat::Qcow2);
            let mut caps = all_capabilities();
            caps.image_formats.qcow2 = false;
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("qcow2")));
        }

        #[test]
        fn additional_disk_format_unsupported() {
            let builder =
                linux_builder().disk(DiskImage::with_format("/disk.qcow2", ImageFormat::Qcow2));
            let mut caps = all_capabilities();
            caps.image_formats.qcow2 = false;
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("qcow2")));
        }
    }

    mod share_validation {
        use super::*;

        #[test]
        fn no_shares() {
            let builder = linux_builder();
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn auto_mechanism_always_passes() {
            let builder = linux_builder().share("/host", "/guest", MountMode::ReadOnly);
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn virtio_fs_supported() {
            let builder = linux_builder().share_with_mechanism(
                "/host",
                "/guest",
                MountMode::ReadOnly,
                ShareMechanism::VirtioFs(VirtioFsConfig::default()),
            );
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn virtio_fs_unsupported() {
            let builder = linux_builder().share_with_mechanism(
                "/host",
                "/guest",
                MountMode::ReadOnly,
                ShareMechanism::VirtioFs(VirtioFsConfig::default()),
            );
            let mut caps = all_capabilities();
            caps.share_mechanisms.virtio_fs = false;
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::UnsupportedFeature(f) if f == "virtio-fs"));
        }

        #[test]
        fn virtio_9p_supported() {
            let builder = linux_builder().share_with_mechanism(
                "/host",
                "/guest",
                MountMode::ReadOnly,
                ShareMechanism::Virtio9p(Virtio9pConfig::default()),
            );
            assert!(builder.validate(&all_capabilities()).is_ok());
        }

        #[test]
        fn virtio_9p_unsupported() {
            let builder = linux_builder().share_with_mechanism(
                "/host",
                "/guest",
                MountMode::ReadOnly,
                ShareMechanism::Virtio9p(Virtio9pConfig::default()),
            );
            let mut caps = all_capabilities();
            caps.share_mechanisms.virtio_9p = false;
            let err = builder.validate(&caps).unwrap_err();
            assert!(matches!(err, Error::UnsupportedFeature(f) if f == "virtio-9p"));
        }
    }

    mod disk_file_validation {
        use super::*;

        #[test]
        fn readonly_exists() {
            let temp_file = tempfile::NamedTempFile::new().unwrap();
            let config = LinuxDirectBootConfig::new("/kernel", "/initrd")
                .with_root_disk(DiskImage::new(temp_file.path()).read_only());
            let builder = VmBuilder::new(config);
            assert!(builder.validate_disk_files().is_ok());
        }

        #[test]
        fn readonly_not_found() {
            let config = LinuxDirectBootConfig::new("/kernel", "/initrd")
                .with_root_disk(DiskImage::new("/nonexistent/path/disk.raw").read_only());
            let builder = VmBuilder::new(config);
            let err = builder.validate_disk_files().unwrap_err();
            assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("not found")));
        }

        #[test]
        fn writable_exists() {
            let temp_file = tempfile::NamedTempFile::new().unwrap();
            let config = LinuxDirectBootConfig::new("/kernel", "/initrd")
                .with_root_disk(DiskImage::new(temp_file.path()));
            let builder = VmBuilder::new(config);
            assert!(builder.validate_disk_files().is_ok());
        }

        #[test]
        fn writable_not_found() {
            let config = LinuxDirectBootConfig::new("/kernel", "/initrd")
                .with_root_disk(DiskImage::new("/nonexistent/path/disk.raw"));
            let builder = VmBuilder::new(config);
            let err = builder.validate_disk_files().unwrap_err();
            assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("not writable")));
        }

        #[test]
        fn additional_disk_readonly_not_found() {
            let builder =
                linux_builder().disk(DiskImage::new("/nonexistent/additional.raw").read_only());
            let err = builder.validate_disk_files().unwrap_err();
            assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("not found")));
        }

        #[test]
        fn additional_disk_writable() {
            let temp_file = tempfile::NamedTempFile::new().unwrap();
            let builder = linux_builder().disk(DiskImage::new(temp_file.path()));
            assert!(builder.validate_disk_files().is_ok());
        }

        #[test]
        fn mixed_root_and_additional() {
            let temp_root = tempfile::NamedTempFile::new().unwrap();
            let temp_additional = tempfile::NamedTempFile::new().unwrap();

            let config = LinuxDirectBootConfig::new("/kernel", "/initrd")
                .with_root_disk(DiskImage::new(temp_root.path()).read_only());
            let builder = VmBuilder::new(config).disk(DiskImage::new(temp_additional.path()));
            assert!(builder.validate_disk_files().is_ok());
        }
    }

    mod builder_methods {
        use super::*;

        #[test]
        fn disk_method_adds_to_disks_vector() {
            let builder = linux_builder()
                .disk(DiskImage::new("/disk1.raw"))
                .disk(DiskImage::new("/disk2.raw"));

            assert!(builder.boot_config.root_disk.is_none());
            assert_eq!(builder.disks.len(), 2);
            assert_eq!(builder.disks[0].path, PathBuf::from("/disk1.raw"));
            assert_eq!(builder.disks[1].path, PathBuf::from("/disk2.raw"));
        }

        #[test]
        fn root_disk_and_additional_disks_separate() {
            let config = LinuxDirectBootConfig::new("/kernel", "/initrd")
                .with_root_disk(DiskImage::new("/root.raw"));
            let builder = VmBuilder::new(config)
                .disk(DiskImage::new("/data1.raw"))
                .disk(DiskImage::new("/data2.raw"));

            assert!(builder.boot_config.root_disk.is_some());
            assert_eq!(
                builder.boot_config.root_disk.as_ref().unwrap().path,
                PathBuf::from("/root.raw")
            );
            assert_eq!(builder.disks.len(), 2);
        }
    }

    mod uefi_specific {
        use super::*;

        #[test]
        fn uefi_boot_disk_exists() {
            let temp_file = tempfile::NamedTempFile::new().unwrap();
            let builder = VmBuilder::new(UefiBootConfig::new(
                DiskImage::new(temp_file.path()).read_only(),
            ));
            assert!(builder.validate_disk_files().is_ok());
        }

        #[test]
        fn uefi_boot_disk_not_found() {
            let builder = VmBuilder::new(UefiBootConfig::new(
                DiskImage::new("/nonexistent/disk.raw").read_only(),
            ));
            let err = builder.validate_disk_files().unwrap_err();
            assert!(matches!(err, Error::InvalidConfig(msg) if msg.contains("not found")));
        }

        #[test]
        fn uefi_efi_store_not_found() {
            let temp_file = tempfile::NamedTempFile::new().unwrap();
            let config = UefiBootConfig::new(DiskImage::new(temp_file.path()).read_only())
                .with_existing_efi_variable_store("/nonexistent/store.efivarstore");
            let builder = VmBuilder::new(config);
            let err = builder.validate_disk_files().unwrap_err();
            assert!(
                matches!(err, Error::InvalidConfig(msg) if msg.contains("EFI variable store not found"))
            );
        }

        #[test]
        fn uefi_efi_store_create_if_missing_ok() {
            let temp_file = tempfile::NamedTempFile::new().unwrap();
            let config = UefiBootConfig::new(DiskImage::new(temp_file.path()).read_only())
                .with_efi_variable_store("/nonexistent/store.efivarstore");
            let builder = VmBuilder::new(config);
            // Should not fail because create_if_missing is true
            assert!(builder.validate_disk_files().is_ok());
        }
    }
}
