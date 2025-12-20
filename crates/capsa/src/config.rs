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
/// let vm = Capsa::linux(config)
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
// TODO: Add Capsa::linux_uefi for UEFI boot support
// TODO: Add Capsa::windows for Windows guest support
pub struct Capsa;

impl Capsa {
    /// Creates a builder for a Linux VM using direct kernel boot.
    ///
    /// Direct boot bypasses the bootloader and boots the kernel directly,
    /// which is faster and simpler for headless Linux VMs. You provide
    /// the kernel image (bzImage) and initrd directly.
    ///
    /// # Arguments
    ///
    /// * `config` - Boot configuration specifying kernel, initrd, and optional root disk
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use capsa::{Capsa, LinuxDirectBootConfig, DiskImage};
    ///
    /// # async fn example() -> capsa::Result<()> {
    /// // Minimal configuration (no persistent storage)
    /// let vm = Capsa::linux(LinuxDirectBootConfig::new("./kernel", "./initrd"))
    ///     .build()
    ///     .await?;
    ///
    /// // With a root filesystem
    /// let config = LinuxDirectBootConfig::new("./kernel", "./initrd")
    ///     .with_root_disk(DiskImage::new("./rootfs.raw"));
    /// let vm = Capsa::linux(config).build().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn linux(config: LinuxDirectBootConfig) -> LinuxVmBuilder {
        LinuxVmBuilder::new(config)
    }
}
