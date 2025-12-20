use crate::builder::LinuxVmBuilder;
use capsa_core::LinuxDirectBootConfig;

/// Entry point for creating virtual machines.
///
/// This struct provides factory methods for creating VM builders configured
/// for different guest operating systems.
pub struct Capsa;

impl Capsa {
    /// Creates a builder for a Linux VM using direct kernel boot.
    ///
    /// Direct boot bypasses the bootloader and boots the kernel directly,
    /// which is faster and simpler for headless Linux VMs.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// use capsa::{Capsa, LinuxDirectBootConfig};
    ///
    /// # async fn example() -> capsa::Result<()> {
    /// let config = LinuxDirectBootConfig::new("./bzImage", "./initrd");
    /// let vm = Capsa::linux(config).build().await?;
    /// # Ok(())
    /// # }
    /// ```
    pub fn linux(config: LinuxDirectBootConfig) -> LinuxVmBuilder {
        LinuxVmBuilder::new(config)
    }
}
