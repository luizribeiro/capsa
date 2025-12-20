use crate::builder::{BootConfigBuilder, VmBuilder};
use crate::pool::Yes;

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
/// - **macOS**: Uses the Virtualization.framework via vfkit
/// - **Linux**: Uses cloud-hypervisor (coming soon)
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
}
