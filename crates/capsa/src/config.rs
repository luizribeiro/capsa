use crate::builder::LinuxVmBuilder;
use crate::pool::Yes;
use capsa_core::LinuxDirectBootConfig;

/// Trait for VM boot configurations.
///
/// Each boot configuration type (e.g., [`LinuxDirectBootConfig`]) implements
/// this trait to specify which builder it produces. This allows [`Capsa::vm`]
/// to accept different configuration types and return the appropriate builder.
///
/// # Implementing for New VM Types
///
/// When adding support for a new VM type (e.g., Windows, UEFI boot), create
/// a new config struct and implement this trait:
///
/// ```ignore
/// impl BootConfig for WindowsBootConfig {
///     type Builder = WindowsVmBuilder;
///
///     fn into_builder(self) -> Self::Builder {
///         WindowsVmBuilder::new(self)
///     }
/// }
/// ```
pub trait BootConfig {
    /// The builder type produced by this configuration.
    type Builder;

    /// Converts this configuration into its corresponding builder.
    fn into_builder(self) -> Self::Builder;
}

impl BootConfig for LinuxDirectBootConfig {
    type Builder = LinuxVmBuilder;

    fn into_builder(self) -> Self::Builder {
        LinuxVmBuilder::new(self)
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
// TODO: Add LinuxUefiBootConfig for UEFI boot support
// TODO: Add WindowsBootConfig for Windows guest support
pub struct Capsa;

impl Capsa {
    /// Creates a builder for a VM with the given boot configuration.
    ///
    /// The configuration type determines which builder is returned.
    /// Currently supports:
    /// - [`LinuxDirectBootConfig`] → [`LinuxVmBuilder`]
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
    pub fn vm<C: BootConfig>(config: C) -> C::Builder {
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
    pub fn pool(config: LinuxDirectBootConfig) -> LinuxVmBuilder<Yes> {
        LinuxVmBuilder::new_pool(config)
    }
}
