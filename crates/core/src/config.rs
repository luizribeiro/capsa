use std::path::PathBuf;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};

/// Network interface configuration for a VM (user-facing).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkInterfaceConfig {
    /// MAC address (auto-generated if None).
    #[serde(default)]
    pub mac: Option<[u8; 6]>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VmConfig {
    pub root: Option<PathBuf>,
    pub kernel: Option<PathBuf>,
    pub initramfs: Option<PathBuf>,
    pub kernel_cmdline: Option<String>,
    pub vcpus: u8,
    pub memory_mib: u32,
    pub verbosity: u8,
    /// Network interfaces. Empty = no networking (default).
    #[serde(default)]
    pub interfaces: Vec<NetworkInterfaceConfig>,
}

impl VmConfig {
    pub fn validate(&self) -> Result<()> {
        if self.interfaces.len() > 1 {
            bail!("multiple network interfaces are not supported yet");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{NetworkInterfaceConfig, VmConfig};

    fn sample_vm_config() -> VmConfig {
        VmConfig {
            root: Some("/tmp/root".into()),
            kernel: None,
            initramfs: None,
            kernel_cmdline: None,
            vcpus: 1,
            memory_mib: 512,
            verbosity: 0,
            interfaces: vec![],
        }
    }

    #[test]
    fn vm_config_validate_rejects_multiple_interfaces() {
        let mut config = sample_vm_config();
        config.interfaces = vec![
            NetworkInterfaceConfig { mac: None },
            NetworkInterfaceConfig { mac: None },
        ];

        let err = config.validate().expect_err("validation should fail");
        assert!(err
            .to_string()
            .contains("multiple network interfaces are not supported yet"));
    }
}
