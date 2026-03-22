use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use crate::{LaunchEnvelope, VmConfig};

impl VmConfig {
    /// Start the VM in the sandboxed sidecar process.
    pub fn start(&self) -> Result<()> {
        self.validate().context("invalid VM configuration")?;

        let vmm_exe = resolve_vmm_binary()?;
        let spec = vm_sandbox_spec(self, &vmm_exe);

        let envelope = LaunchEnvelope {
            vm_config: self.clone(),
            // Interface fd resolution/fd passing is added in a later commit.
            resolved_interfaces: vec![],
        };
        let envelope_json =
            serde_json::to_string(&envelope).context("failed to serialize launch envelope")?;
        let child_args = vec!["--launch-envelope-json".to_string(), envelope_json];

        let child =
            capsa_sandbox::spawn_sandboxed(&vmm_exe, &child_args, &spec).with_context(|| {
                format!(
                    "failed to spawn sandboxed VMM process: {}",
                    vmm_exe.display()
                )
            })?;

        let status = child.wait().context("failed to wait on sandboxed child")?;
        if status.success() {
            return Ok(());
        }

        anyhow::bail!("sandboxed VMM process exited with status {status}")
    }
}

fn vm_sandbox_spec(config: &VmConfig, vmm_exe: &Path) -> capsa_sandbox::SandboxSpec {
    let mut spec = capsa_sandbox::SandboxSpec::new().allow_network(true);

    spec.read_only_paths.push(vmm_exe.to_path_buf());

    if let Some(root) = &config.root {
        spec.read_write_paths.push(root.clone());
    }

    if let Some(kernel) = &config.kernel {
        spec.read_only_paths.push(kernel.clone());
    }

    if let Some(initramfs) = &config.initramfs {
        spec.read_only_paths.push(initramfs.clone());
    }

    spec
}

fn resolve_vmm_binary() -> Result<PathBuf> {
    if let Some(path) = std::env::var_os("CAPSA_VMM_PATH") {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }

    if let Ok(current_exe) = std::env::current_exe() {
        let sibling = current_exe.with_file_name("capsa-vmm");
        if sibling.exists() {
            return Ok(sibling);
        }
    }

    if let Some(in_path) = find_in_path("capsa-vmm") {
        return Ok(in_path);
    }

    anyhow::bail!(
        "unable to locate capsa-vmm sidecar. Build/install it (e.g. `cargo build --bins`) and optionally set CAPSA_VMM_PATH"
    )
}

fn find_in_path(binary_name: &str) -> Option<PathBuf> {
    let path_var = std::env::var_os("PATH")?;
    for dir in std::env::split_paths(&path_var) {
        let candidate = dir.join(binary_name);
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}
