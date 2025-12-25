use super::vm_builder::{BootConfigBuilder, VmBuilder};
use crate::pool::No;
use capsa_core::{
    BackendCapabilities, BootMethod, DiskImage, Error, HypervisorBackend, KernelCmdline,
    LinuxDirectBootConfig, NetworkMode, ResourceConfig, Result, SharedDir, UefiBootConfig,
    VmConfig, VsockConfig,
};
use std::path::PathBuf;
use uuid::Uuid;

// ============================================================================
// LinuxDirectBootConfig implementation
// ============================================================================

impl BootConfigBuilder for LinuxDirectBootConfig {
    fn validate_boot(&self, capabilities: &BackendCapabilities) -> Result<()> {
        if !capabilities.boot_methods.linux_direct {
            return Err(Error::UnsupportedFeature(
                "boot method: linux direct".into(),
            ));
        }
        Ok(())
    }

    fn validate_boot_disk_files(&self) -> Result<()> {
        if let Some(disk) = &self.root_disk {
            validate_disk_file(disk)?;
        }
        Ok(())
    }

    fn boot_disk(&self) -> Option<&DiskImage> {
        self.root_disk.as_ref()
    }

    fn into_vm_config(
        self,
        disks: Vec<DiskImage>,
        resources: ResourceConfig,
        shares: Vec<SharedDir>,
        network: NetworkMode,
        console_enabled: bool,
        vsock: VsockConfig,
        backend: &dyn HypervisorBackend,
    ) -> (VmConfig, Option<PathBuf>) {
        let cmdline = generate_cmdline(&self.cmdline, self.root_disk.is_some(), backend);

        let config = VmConfig {
            boot: BootMethod::LinuxDirect {
                kernel: self.kernel,
                initrd: self.initrd,
                cmdline,
            },
            root_disk: self.root_disk,
            disks,
            resources,
            shares,
            network,
            console_enabled,
            vsock,
            cluster_network_fd: None,
        };
        (config, None)
    }
}

fn generate_cmdline(
    user_cmdline: &KernelCmdline,
    has_root_disk: bool,
    backend: &dyn HypervisorBackend,
) -> String {
    if user_cmdline.is_overridden() {
        return user_cmdline.build();
    }

    let mut cmdline = KernelCmdline::new();
    cmdline.merge(&backend.kernel_cmdline_defaults());

    if has_root_disk {
        cmdline.root(backend.default_root_device());
    }

    cmdline.merge(user_cmdline);

    cmdline.build()
}

/// Linux-specific builder methods.
impl<P> VmBuilder<LinuxDirectBootConfig, P> {
    /// Adds a kernel command line argument (key=value pair).
    pub fn cmdline_arg(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.boot_config.cmdline.arg(key, value);
        self
    }

    /// Adds a kernel command line flag (single value without =).
    pub fn cmdline_flag(mut self, name: impl Into<String>) -> Self {
        self.boot_config.cmdline.flag(name);
        self
    }

    /// Overrides the entire kernel command line with a custom string.
    ///
    /// This replaces all default arguments. Use with caution.
    pub fn cmdline_override(mut self, cmdline: impl Into<String>) -> Self {
        self.boot_config.cmdline.override_with(cmdline);
        self
    }
}

// ============================================================================
// UefiBootConfig implementation
// ============================================================================

impl BootConfigBuilder for UefiBootConfig {
    fn validate_boot(&self, capabilities: &BackendCapabilities) -> Result<()> {
        if !capabilities.boot_methods.uefi {
            return Err(Error::UnsupportedFeature("boot method: uefi".into()));
        }
        Ok(())
    }

    fn validate_boot_disk_files(&self) -> Result<()> {
        validate_disk_file(&self.disk)?;

        // Validate EFI variable store if specified and not creating
        if let Some(store) = &self.efi_variable_store
            && !store.create_if_missing
            && !store.path.exists()
        {
            return Err(Error::InvalidConfig(format!(
                "EFI variable store not found: {}",
                store.path.display()
            )));
        }

        Ok(())
    }

    fn boot_disk(&self) -> Option<&DiskImage> {
        Some(&self.disk)
    }

    fn into_vm_config(
        self,
        disks: Vec<DiskImage>,
        resources: ResourceConfig,
        shares: Vec<SharedDir>,
        network: NetworkMode,
        console_enabled: bool,
        vsock: VsockConfig,
        _backend: &dyn HypervisorBackend,
    ) -> (VmConfig, Option<PathBuf>) {
        let (efi_store, temp_file) = resolve_efi_variable_store(&self.efi_variable_store);
        let create = self
            .efi_variable_store
            .as_ref()
            .map(|s| s.create_if_missing)
            .unwrap_or(true);

        let config = VmConfig {
            boot: BootMethod::Uefi {
                efi_variable_store: efi_store,
                create_variable_store: create,
            },
            root_disk: Some(self.disk),
            disks,
            resources,
            shares,
            network,
            console_enabled,
            vsock,
            cluster_network_fd: None,
        };
        (config, temp_file)
    }
}

/// Generates a unique temp path for an EFI variable store.
pub(crate) fn generate_temp_efi_store_path() -> PathBuf {
    std::env::temp_dir().join(format!("capsa-efi-{}.efivarstore", Uuid::new_v4()))
}

fn resolve_efi_variable_store(
    store: &Option<capsa_core::EfiVariableStore>,
) -> (PathBuf, Option<PathBuf>) {
    if let Some(store) = store {
        (store.path.clone(), None)
    } else {
        let temp_path = generate_temp_efi_store_path();
        (temp_path.clone(), Some(temp_path))
    }
}

// ============================================================================
// Shared helpers
// ============================================================================

fn validate_disk_file(disk: &DiskImage) -> Result<()> {
    if disk.read_only {
        if !disk.path.exists() {
            return Err(Error::InvalidConfig(format!(
                "disk not found: {}",
                disk.path.display()
            )));
        }
    } else {
        std::fs::OpenOptions::new()
            .write(true)
            .open(&disk.path)
            .map_err(|e| {
                Error::InvalidConfig(format!("disk not writable: {}: {}", disk.path.display(), e))
            })?;
    }
    Ok(())
}

// ============================================================================
// Type aliases for backwards compatibility
// ============================================================================

/// Builder for Linux VMs using direct kernel boot.
pub type LinuxVmBuilder<P = No> = VmBuilder<LinuxDirectBootConfig, P>;

/// Builder for VMs using UEFI boot.
pub type UefiVmBuilder<P = No> = VmBuilder<UefiBootConfig, P>;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_temp_efi_store_path_creates_unique_paths() {
        let path1 = generate_temp_efi_store_path();
        let path2 = generate_temp_efi_store_path();

        assert_ne!(path1, path2, "Each call should generate a unique path");
    }

    #[test]
    fn generate_temp_efi_store_path_uses_temp_dir() {
        let path = generate_temp_efi_store_path();
        let temp_dir = std::env::temp_dir();

        assert!(
            path.starts_with(&temp_dir),
            "Path should be in temp directory"
        );
    }

    #[test]
    fn generate_temp_efi_store_path_has_correct_extension() {
        let path = generate_temp_efi_store_path();
        let filename = path.file_name().unwrap().to_str().unwrap();

        assert!(
            filename.ends_with(".efivarstore"),
            "Path should have .efivarstore extension"
        );
        assert!(
            filename.starts_with("capsa-efi-"),
            "Path should have capsa-efi- prefix"
        );
    }

    #[test]
    fn resolve_efi_variable_store_with_user_provided_store() {
        let user_path = PathBuf::from("/user/provided/store.efivarstore");
        let store = Some(capsa_core::EfiVariableStore {
            path: user_path.clone(),
            create_if_missing: false,
        });

        let (path, temp_file) = resolve_efi_variable_store(&store);

        assert_eq!(path, user_path);
        assert!(
            temp_file.is_none(),
            "User-provided store should not be tracked as temp file"
        );
    }

    #[test]
    fn resolve_efi_variable_store_without_store_generates_temp() {
        let (path, temp_file) = resolve_efi_variable_store(&None);

        assert!(path.to_str().unwrap().contains("capsa-efi-"));
        assert!(
            temp_file.is_some(),
            "Auto-generated store should be tracked as temp file"
        );
        assert_eq!(path, temp_file.unwrap());
    }
}
