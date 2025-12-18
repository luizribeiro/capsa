use crate::backend::{select_backend, HypervisorBackend, InternalVmConfig};
use crate::boot::{KernelCmdline, LinuxDirectBootConfig};
use crate::capabilities::BackendCapabilities;
use crate::error::{Error, Result};
use crate::handle::VmHandle;
use crate::types::{
    ConsoleMode, DiskImage, GuestOs, MountMode, NetworkMode, ResourceConfig, ShareMechanism,
    SharedDir,
};
use std::path::PathBuf;
use std::time::Duration;

pub struct LinuxVmBuilder {
    config: LinuxDirectBootConfig,
    resources: ResourceConfig,
    shares: Vec<SharedDir>,
    network: NetworkMode,
    console: ConsoleMode,
    cmdline: KernelCmdline,
    #[allow(dead_code)]
    timeout: Option<Duration>,
}

impl LinuxVmBuilder {
    pub fn new(config: LinuxDirectBootConfig) -> Self {
        Self {
            config,
            resources: ResourceConfig::default(),
            shares: Vec::new(),
            network: NetworkMode::default(),
            console: ConsoleMode::default(),
            cmdline: KernelCmdline::new(),
            timeout: None,
        }
    }

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

    pub fn disk(mut self, disk: DiskImage) -> Self {
        self.config.disk = Some(disk);
        self
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

    pub fn cmdline_arg(
        mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Self {
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

        Ok(VmHandle::new(backend_handle, GuestOs::Linux, self.resources))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::capabilities::{BackendCapabilities, ShareMechanismSupport};
    use crate::types::{MountMode, Virtio9pConfig, VirtioFsConfig};
    use std::path::PathBuf;

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
        }
    }

    #[test]
    fn validate_no_shares() {
        let builder = builder_with_shares(vec![]);
        let caps = BackendCapabilities::default();
        assert!(builder.validate(&caps).is_ok());
    }

    #[test]
    fn validate_auto_mechanism_always_passes() {
        let builder = builder_with_shares(vec![SharedDir::new("/host", "/guest", MountMode::ReadOnly)]);
        let caps = BackendCapabilities::default();
        assert!(builder.validate(&caps).is_ok());
    }

    #[test]
    fn validate_virtio_fs_supported() {
        let builder = builder_with_shares(vec![SharedDir::with_mechanism(
            "/host",
            "/guest",
            MountMode::ReadOnly,
            ShareMechanism::VirtioFs(VirtioFsConfig::default()),
        )]);
        let caps = BackendCapabilities {
            share_mechanisms: ShareMechanismSupport {
                virtio_fs: true,
                virtio_9p: false,
            },
            ..Default::default()
        };
        assert!(builder.validate(&caps).is_ok());
    }

    #[test]
    fn validate_virtio_fs_unsupported() {
        let builder = builder_with_shares(vec![SharedDir::with_mechanism(
            "/host",
            "/guest",
            MountMode::ReadOnly,
            ShareMechanism::VirtioFs(VirtioFsConfig::default()),
        )]);
        let caps = BackendCapabilities::default();
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
        let caps = BackendCapabilities {
            share_mechanisms: ShareMechanismSupport {
                virtio_fs: false,
                virtio_9p: true,
            },
            ..Default::default()
        };
        assert!(builder.validate(&caps).is_ok());
    }

    #[test]
    fn validate_virtio_9p_unsupported() {
        let builder = builder_with_shares(vec![SharedDir::with_mechanism(
            "/host",
            "/guest",
            MountMode::ReadOnly,
            ShareMechanism::Virtio9p(Virtio9pConfig::default()),
        )]);
        let caps = BackendCapabilities::default();
        let err = builder.validate(&caps).unwrap_err();
        assert!(matches!(err, Error::UnsupportedFeature(f) if f == "virtio-9p"));
    }
}
