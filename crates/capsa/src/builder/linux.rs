use crate::backend::{HypervisorBackend, InternalVmConfig, select_backend};
use crate::boot::{KernelCmdline, LinuxDirectBootConfig};
use crate::capabilities::BackendCapabilities;
use crate::error::{Error, Result};
use crate::handle::VmHandle;
use crate::pool::{No, Poolability, VmPool, Yes};
use crate::types::{
    ConsoleMode, DiskImage, GuestOs, ImageFormat, MountMode, NetworkMode, ResourceConfig,
    ShareMechanism, SharedDir,
};
use std::path::PathBuf;
use std::time::Duration;

pub struct LinuxVmBuilder<P = Yes> {
    config: LinuxDirectBootConfig,
    resources: ResourceConfig,
    shares: Vec<SharedDir>,
    network: NetworkMode,
    console: ConsoleMode,
    cmdline: KernelCmdline,
    #[allow(dead_code)]
    timeout: Option<Duration>,
    #[allow(dead_code)]
    poolable: Poolability<P>,
}

impl LinuxVmBuilder<Yes> {
    pub fn new(config: LinuxDirectBootConfig) -> Self {
        Self {
            config,
            resources: ResourceConfig::default(),
            shares: Vec::new(),
            network: NetworkMode::default(),
            console: ConsoleMode::default(),
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        }
    }

    pub async fn build_pool(self, size: usize) -> Result<VmPool> {
        let backend = select_backend()?;
        self.validate(backend.capabilities())?;

        let cmdline = self.generate_cmdline(backend.as_ref());

        let internal_config = InternalVmConfig {
            kernel: self.config.kernel,
            initrd: self.config.initrd,
            disk: self.config.disk,
            cmdline,
            resources: self.resources,
            shares: self.shares,
            network: self.network,
            console: self.console,
        };

        VmPool::new(internal_config, size).await
    }
}

impl<P> LinuxVmBuilder<P> {
    pub fn cpus(mut self, count: u32) -> Self {
        self.resources.cpus = count;
        self
    }

    pub fn memory_mb(mut self, mb: u32) -> Self {
        self.resources.memory_mb = mb;
        self
    }

    pub fn timeout(mut self, duration: Duration) -> Self {
        self.timeout = Some(duration);
        self
    }

    pub fn disk(mut self, disk: DiskImage) -> LinuxVmBuilder<No> {
        self.config.disk = Some(disk);
        LinuxVmBuilder {
            config: self.config,
            resources: self.resources,
            shares: self.shares,
            network: self.network,
            console: self.console,
            cmdline: self.cmdline,
            timeout: self.timeout,
            poolable: Poolability::new(),
        }
    }

    pub fn share(
        mut self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
    ) -> Self {
        self.shares.push(SharedDir::new(host, guest, mode));
        self
    }

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

    pub fn shares(mut self, shares: impl IntoIterator<Item = SharedDir>) -> Self {
        self.shares.extend(shares);
        self
    }

    pub fn network(mut self, mode: NetworkMode) -> Self {
        self.network = mode;
        self
    }

    pub fn no_network(self) -> Self {
        self.network(NetworkMode::None)
    }

    pub fn console(mut self, mode: ConsoleMode) -> Self {
        self.console = mode;
        self
    }

    pub fn console_enabled(self) -> Self {
        self.console(ConsoleMode::Enabled)
    }

    pub fn console_stdio(self) -> Self {
        self.console(ConsoleMode::Stdio)
    }

    pub fn cmdline_arg(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.cmdline.arg(key, value);
        self
    }

    pub fn cmdline_flag(mut self, name: impl Into<String>) -> Self {
        self.cmdline.flag(name);
        self
    }

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

        if let Some(disk) = &self.config.disk {
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

    fn generate_cmdline(&self, backend: &dyn HypervisorBackend) -> String {
        if self.cmdline.is_overridden() {
            return self.cmdline.build();
        }

        let mut cmdline = KernelCmdline::new();
        cmdline.merge(&backend.kernel_cmdline_defaults());

        // Only set root device if we have a disk
        if self.config.disk.is_some() {
            cmdline.root(backend.default_root_device());
        }

        cmdline.merge(&self.cmdline);

        cmdline.build()
    }

    pub async fn build(self) -> Result<VmHandle> {
        let backend = select_backend()?;
        self.validate(backend.capabilities())?;

        let cmdline = self.generate_cmdline(backend.as_ref());

        let internal_config = InternalVmConfig {
            kernel: self.config.kernel,
            initrd: self.config.initrd,
            disk: self.config.disk,
            cmdline,
            resources: self.resources.clone(),
            shares: self.shares,
            network: self.network,
            console: self.console,
        };

        let backend_handle = backend.start(&internal_config).await?;

        Ok(VmHandle::new(
            backend_handle,
            GuestOs::Linux,
            self.resources,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{
        BackendCapabilities, BootMethodSupport, ImageFormatSupport, NetworkModeSupport,
        ShareMechanismSupport,
    };
    use crate::types::{MountMode, Virtio9pConfig, VirtioFsConfig};
    use std::path::PathBuf;

    fn builder_with_network(network: NetworkMode) -> LinuxVmBuilder {
        LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                disk: None,
            },
            resources: ResourceConfig::default(),
            shares: vec![],
            network,
            console: ConsoleMode::default(),
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
                disk: None,
            },
            resources: ResourceConfig { cpus, memory_mb },
            shares: vec![],
            network: NetworkMode::default(),
            console: ConsoleMode::default(),
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
                disk: None,
            },
            resources: ResourceConfig::default(),
            shares,
            network: NetworkMode::default(),
            console: ConsoleMode::default(),
            cmdline: KernelCmdline::new(),
            timeout: None,
            poolable: Poolability::new(),
        }
    }

    fn builder_with_disk(format: ImageFormat) -> LinuxVmBuilder {
        LinuxVmBuilder {
            config: LinuxDirectBootConfig {
                kernel: PathBuf::from("/kernel"),
                initrd: PathBuf::from("/initrd"),
                disk: Some(DiskImage::with_format("/disk.img", format)),
            },
            resources: ResourceConfig::default(),
            shares: vec![],
            network: NetworkMode::default(),
            console: ConsoleMode::default(),
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
        let builder = builder_with_disk(ImageFormat::Raw);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_raw_disk_unsupported() {
        let builder = builder_with_disk(ImageFormat::Raw);
        let mut caps = all_capabilities();
        caps.image_formats.raw = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("raw")));
    }

    #[test]
    fn validate_qcow2_disk_supported() {
        let builder = builder_with_disk(ImageFormat::Qcow2);
        assert!(builder.validate(&all_capabilities()).is_ok());
    }

    #[test]
    fn validate_qcow2_disk_unsupported() {
        let builder = builder_with_disk(ImageFormat::Qcow2);
        let mut caps = all_capabilities();
        caps.image_formats.qcow2 = false;
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f.contains("qcow2")));
    }
}
