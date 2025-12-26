use crate::builder::{BootConfigBuilder, VmBuilder};
use crate::pool::Yes;
use crate::sandbox::{NoMainProcess, SandboxBuilder};

/// Trait for VM boot configurations.
///
/// Each boot configuration type (e.g., [`LinuxDirectBootConfig`](capsa_core::LinuxDirectBootConfig))
/// implements this trait to specify which builder it produces. This allows [`Capsa::vm`]
/// and [`Capsa::pool`] to accept different configuration types and return
/// the appropriate builder.
pub trait BootConfig: BootConfigBuilder {
    /// Converts this configuration into a single VM builder.
    fn into_builder(self) -> VmBuilder<Self, crate::pool::No>;
    /// Converts this configuration into a pool builder.
    fn into_pool_builder(self) -> VmBuilder<Self, Yes>;
}

impl<B: BootConfigBuilder> BootConfig for B {
    fn into_builder(self) -> VmBuilder<Self, crate::pool::No> {
        VmBuilder::new(self)
    }

    fn into_pool_builder(self) -> VmBuilder<Self, Yes> {
        VmBuilder::new_pool(self)
    }
}

/// Entry point for creating virtual machines.
///
/// `Capsa` is the starting point for all VM creation. It provides factory
/// methods that return builders for configuring and launching VMs.
///
/// # Supported Platforms
///
/// - **macOS**: Uses Apple's Virtualization.framework
/// - **Linux**: Uses KVM
///
/// # Example
///
/// ```rust,no_run
/// use capsa::{Capsa, LinuxDirectBootConfig, MountMode};
///
/// # async fn example() -> capsa::Result<()> {
/// let config = LinuxDirectBootConfig::new("./kernel", "./initrd")
///     .with_root_disk("./rootfs.raw");
///
/// let vm = Capsa::vm(config)
///     .cpus(2)
///     .memory_mb(1024)
///     .share("./workspace", "/mnt", MountMode::ReadWrite)
///     .console_enabled()
///     .build()
///     .await?;
///
/// let console = vm.console().await?;
/// console.wait_for("login:").await?;
/// # Ok(())
/// # }
/// ```
pub struct Capsa;

impl Capsa {
    /// Creates a builder for a VM with the given boot configuration.
    ///
    /// The configuration type determines which builder is returned.
    /// Supports:
    /// - [`LinuxDirectBootConfig`] → [`LinuxVmBuilder`] (fast Linux boot)
    /// - [`UefiBootConfig`] → [`UefiVmBuilder`] (UEFI boot for any OS)
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use capsa::{Capsa, LinuxDirectBootConfig};
    ///
    /// # async fn example() -> capsa::Result<()> {
    /// let config = LinuxDirectBootConfig::new("./kernel", "./initrd")
    ///     .with_root_disk("./rootfs.raw");
    /// let vm = Capsa::vm(config).build().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn vm<C: BootConfig>(config: C) -> VmBuilder<C, crate::pool::No> {
        config.into_builder()
    }

    /// Creates a builder for a VM pool with the given boot configuration.
    ///
    /// Pools pre-start multiple identical VMs that can be reserved and released.
    /// This is useful when you need to run many short-lived workloads.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use capsa::{Capsa, LinuxDirectBootConfig};
    ///
    /// # async fn example() -> capsa::Result<()> {
    /// let config = LinuxDirectBootConfig::new("./kernel", "./initrd");
    ///
    /// let pool = Capsa::pool(config)
    ///     .cpus(2)
    ///     .memory_mb(512)
    ///     .build(5)
    ///     .await?;
    ///
    /// let vm = pool.reserve().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn pool<C: BootConfig>(config: C) -> VmBuilder<C, Yes> {
        config.into_pool_builder()
    }

    /// Creates a builder for a sandbox VM.
    ///
    /// A sandbox is a VM with a capsa-controlled kernel and initrd that provides
    /// guaranteed features:
    /// - Auto-mounting of shared directories
    /// - Main process support via `.run()` or `.oci()`
    /// - Guest agent for structured command execution
    /// - Known environment with predictable capabilities
    ///
    /// Unlike raw VMs, the sandbox requires specifying a main process via
    /// `.run()` or `.oci()` before calling `.build()`.
    ///
    /// # Example
    ///
    /// ```rust,ignore
    /// use capsa::{Capsa, MountMode};
    ///
    /// let vm = Capsa::sandbox()
    ///     .share("./workspace", "/mnt", MountMode::ReadWrite)
    ///     .cpus(2)
    ///     .memory_mb(1024)
    ///     .run("/bin/sh", &["-c", "ls /mnt"])
    ///     .build()
    ///     .await?;
    ///
    /// vm.wait_ready().await?;
    /// let result = vm.exec("ls /mnt").await?;
    /// ```
    pub fn sandbox() -> SandboxBuilder<NoMainProcess> {
        SandboxBuilder::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sandbox_returns_builder() {
        let builder = Capsa::sandbox();
        assert!(builder.main_process.is_none());
    }

    #[test]
    fn sandbox_builder_can_chain_share() {
        use capsa_core::MountMode;

        let builder = Capsa::sandbox()
            .share("./workspace", "/mnt", MountMode::ReadWrite)
            .cpus(4)
            .memory_mb(2048);

        assert_eq!(builder.shares.len(), 1);
        assert_eq!(builder.resources.cpus, 4);
        assert_eq!(builder.resources.memory_mb, 2048);
    }

    #[test]
    fn sandbox_builder_can_chain_run() {
        let builder = Capsa::sandbox().run("/bin/sh", &["-c", "echo hello"]);
        assert!(builder.main_process.is_some());
    }
}
