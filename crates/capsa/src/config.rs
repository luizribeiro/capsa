use crate::builder::LinuxVmBuilder;
use capsa_core::LinuxDirectBootConfig;

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
/// use capsa::{Capsa, LinuxDirectBootConfig, DiskImage, MountMode};
///
/// # async fn example() -> capsa::Result<()> {
/// // Configure boot settings
/// let config = LinuxDirectBootConfig::new("./kernel", "./initrd")
///     .with_root_disk(DiskImage::new("./rootfs.raw"));
///
/// // Build and start the VM
/// let vm = Capsa::vm(config)
///     .cpus(2)
///     .memory_mb(1024)
///     .share("./workspace", "/mnt", MountMode::ReadWrite)
///     .console_enabled()
///     .build()
///     .await?;
///
/// // Interact with the VM
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
    /// The type of VM is determined by the configuration passed in.
    /// Currently supports Linux direct boot via [`LinuxDirectBootConfig`].
    ///
    /// # Arguments
    ///
    /// * `config` - Boot configuration specifying how to boot the VM
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use capsa::{Capsa, LinuxDirectBootConfig, DiskImage};
    ///
    /// # async fn example() -> capsa::Result<()> {
    /// // Minimal configuration (no persistent storage)
    /// let vm = Capsa::vm(LinuxDirectBootConfig::new("./kernel", "./initrd"))
    ///     .build()
    ///     .await?;
    ///
    /// // With a root filesystem
    /// let config = LinuxDirectBootConfig::new("./kernel", "./initrd")
    ///     .with_root_disk(DiskImage::new("./rootfs.raw"));
    /// let vm = Capsa::vm(config).build().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn vm(config: LinuxDirectBootConfig) -> LinuxVmBuilder {
        LinuxVmBuilder::new(config)
    }
}
