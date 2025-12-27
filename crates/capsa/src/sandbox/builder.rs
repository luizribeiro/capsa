//! Sandbox builder with typestate pattern.

use super::config::{CapsaSandboxConfig, MainProcess, ShareConfig};
use crate::backend::select_backend;
use crate::handle::VmHandle;
use capsa_core::{
    BootMethod, Error, GuestOs, MountMode, NetworkMode, ResourceConfig, Result, SharedDir,
    VmConfig, VsockConfig,
};
use capsa_sandbox_protocol::AGENT_VSOCK_PORT;
use std::marker::PhantomData;
use std::path::PathBuf;

/// Marker: no main process specified yet.
pub struct NoMainProcess;

/// Marker: main process has been specified.
pub struct HasMainProcess;

/// Builder for sandbox VMs with typestate for main process.
///
/// The typestate pattern enforces at compile time that:
/// - `.run()` and `.oci()` are mutually exclusive
/// - `.build()` is only available after specifying a main process
///
/// # Example
///
/// ```rust,ignore
/// let vm = Capsa::sandbox()
///     .share("./workspace", "/mnt", MountMode::ReadWrite)
///     .run("/bin/sh", &[])
///     .build()
///     .await?;
/// ```
pub struct SandboxBuilder<M = NoMainProcess> {
    pub(crate) config: CapsaSandboxConfig,
    pub(crate) shares: Vec<ShareConfig>,
    #[allow(dead_code)] // Will be used in build() implementation
    pub(crate) main_process: Option<MainProcess>,
    pub(crate) resources: ResourceConfig,
    pub(crate) network: NetworkMode,
    pub(crate) console_enabled: bool,
    pub(crate) vsock: VsockConfig,
    pub(crate) _marker: PhantomData<M>,
}

impl SandboxBuilder<NoMainProcess> {
    /// Creates a new sandbox builder.
    pub fn new() -> Self {
        Self {
            config: CapsaSandboxConfig::new(),
            shares: Vec::new(),
            main_process: None,
            resources: ResourceConfig::default(),
            network: NetworkMode::default(),
            console_enabled: true,
            vsock: VsockConfig::default(),
            _marker: PhantomData,
        }
    }

    /// Run a binary as the main process.
    ///
    /// Can only be called once, and cannot be combined with `.oci()`.
    pub fn run(self, path: impl Into<String>, args: &[&str]) -> SandboxBuilder<HasMainProcess> {
        SandboxBuilder {
            config: self.config,
            shares: self.shares,
            main_process: Some(MainProcess::run(path, args)),
            resources: self.resources,
            network: self.network,
            console_enabled: self.console_enabled,
            vsock: self.vsock,
            _marker: PhantomData,
        }
    }

    /// Run an OCI container as the main process.
    ///
    /// Can only be called once, and cannot be combined with `.run()`.
    pub fn oci(self, image: impl Into<String>, args: &[&str]) -> SandboxBuilder<HasMainProcess> {
        SandboxBuilder {
            config: self.config,
            shares: self.shares,
            main_process: Some(MainProcess::oci(image, args)),
            resources: self.resources,
            network: self.network,
            console_enabled: self.console_enabled,
            vsock: self.vsock,
            _marker: PhantomData,
        }
    }
}

impl Default for SandboxBuilder<NoMainProcess> {
    fn default() -> Self {
        Self::new()
    }
}

impl<M> SandboxBuilder<M> {
    /// Share a directory with automatic mounting.
    ///
    /// The directory will be mounted at the specified guest path when the
    /// sandbox boots.
    pub fn share(
        mut self,
        host: impl Into<PathBuf>,
        guest: impl Into<String>,
        mode: MountMode,
    ) -> Self {
        self.shares.push(ShareConfig {
            host_path: host.into(),
            guest_path: guest.into(),
            read_only: mode == MountMode::ReadOnly,
        });
        self
    }

    /// Sets the number of virtual CPUs for the VM.
    pub fn cpus(mut self, count: u32) -> Self {
        self.resources.cpus = count;
        self
    }

    /// Sets the amount of memory in megabytes for the VM.
    pub fn memory_mb(mut self, mb: u32) -> Self {
        self.resources.memory_mb = mb;
        self
    }

    /// Sets the network mode for the VM.
    pub fn network(mut self, mode: NetworkMode) -> Self {
        self.network = mode;
        self
    }

    /// Disables networking for the VM.
    pub fn no_network(self) -> Self {
        self.network(NetworkMode::None)
    }

    /// Overrides the default sandbox kernel.
    pub fn kernel(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.kernel_override = Some(path.into());
        self
    }

    /// Overrides the default sandbox initrd.
    pub fn initrd(mut self, path: impl Into<PathBuf>) -> Self {
        self.config.initrd_override = Some(path.into());
        self
    }
}

impl SandboxBuilder<HasMainProcess> {
    /// Resolves kernel and initrd paths.
    ///
    /// Priority:
    /// 1. Explicit overrides via `.kernel()` / `.initrd()`
    /// 2. Automatic lookup from test-vms manifest (when test-utils feature enabled)
    fn resolve_kernel_initrd(&self) -> Result<(PathBuf, PathBuf)> {
        // Use explicit overrides if provided
        if let (Some(kernel), Some(initrd)) =
            (&self.config.kernel_override, &self.config.initrd_override)
        {
            return Ok((kernel.clone(), initrd.clone()));
        }

        // Try automatic lookup from manifest
        #[cfg(feature = "test-utils")]
        {
            if let Some((kernel, initrd)) = self.try_load_from_manifest() {
                return Ok((kernel, initrd));
            }
        }

        Err(Error::InvalidConfig(
            "sandbox kernel/initrd not found - either use .kernel()/.initrd() or ensure \
             test-vms are built with 'nix-build nix/test-vms -A x86_64 -o result-vms'"
                .into(),
        ))
    }

    #[cfg(feature = "test-utils")]
    fn try_load_from_manifest(&self) -> Option<(PathBuf, PathBuf)> {
        // TODO: For production release, kernel/initrd should be packaged with the binary.
        // For now, we look for them relative to the crate's manifest directory.
        let manifest_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let result_vms = manifest_dir.parent()?.parent()?.join("result-vms");
        let manifest_path = result_vms.join("manifest.json");

        let content = std::fs::read_to_string(&manifest_path).ok()?;
        let manifest: std::collections::HashMap<String, serde_json::Value> =
            serde_json::from_str(&content).ok()?;

        let sandbox = manifest.get("sandbox")?;
        let kernel_rel = sandbox.get("kernel")?.as_str()?;
        let initrd_rel = sandbox.get("initrd")?.as_str()?;

        let kernel = result_vms.join(kernel_rel);
        let initrd = result_vms.join(initrd_rel);

        if kernel.exists() && initrd.exists() {
            Some((kernel, initrd))
        } else {
            None
        }
    }

    /// Builds and starts the sandbox VM.
    ///
    /// Automatically uses the sandbox kernel/initrd from the test-vms manifest
    /// when available. Use `.kernel()` and `.initrd()` to override.
    pub async fn build(self) -> Result<VmHandle> {
        let (kernel, initrd) = self.resolve_kernel_initrd()?;
        let cmdline = self.generate_cmdline();
        let shares = self.generate_shares();

        let socket_path = generate_temp_vsock_path(AGENT_VSOCK_PORT);
        let mut vsock = self.vsock;
        vsock.add_port(capsa_core::VsockPortConfig::connect(
            AGENT_VSOCK_PORT,
            socket_path,
        ));
        let vsock_for_handle = vsock.clone();

        let config = VmConfig {
            boot: BootMethod::LinuxDirect {
                kernel,
                initrd,
                cmdline,
            },
            root_disk: None,
            disks: Vec::new(),
            resources: self.resources.clone(),
            shares,
            network: self.network,
            console_enabled: self.console_enabled,
            vsock,
            cluster_network_fd: None,
        };

        let backend = select_backend()?;
        let backend_handle = backend.start(&config).await?;

        Ok(
            VmHandle::new(backend_handle, GuestOs::Linux, self.resources)
                .with_vsock_config(&vsock_for_handle),
        )
    }

    fn generate_cmdline(&self) -> String {
        use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};

        let mut parts = vec![
            "console=hvc0".to_string(),
            "reboot=t".to_string(),
            "panic=-1".to_string(),
            "threadirqs".to_string(),
            "acpi=off".to_string(),
            "quiet".to_string(),
        ];

        for (i, share) in self.shares.iter().enumerate() {
            let tag = format!("share{}", i);
            parts.push(format!("capsa.mount={}:{}", tag, share.guest_path));
        }

        match &self.main_process {
            Some(MainProcess::Run { path, args }) => {
                // Percent-encode each argument to handle spaces and special characters
                let encoded_path = utf8_percent_encode(path, NON_ALPHANUMERIC).to_string();
                let encoded_args: Vec<String> = args
                    .iter()
                    .map(|s| utf8_percent_encode(s, NON_ALPHANUMERIC).to_string())
                    .collect();
                let mut run_parts = vec![encoded_path];
                run_parts.extend(encoded_args);
                parts.push(format!("capsa.run={}", run_parts.join(":")));
            }
            Some(MainProcess::Oci { image, args }) => {
                let encoded_image = utf8_percent_encode(image, NON_ALPHANUMERIC).to_string();
                let encoded_args: Vec<String> = args
                    .iter()
                    .map(|s| utf8_percent_encode(s, NON_ALPHANUMERIC).to_string())
                    .collect();
                let mut oci_parts = vec![encoded_image];
                oci_parts.extend(encoded_args);
                parts.push(format!("capsa.oci={}", oci_parts.join(":")));
            }
            None => unreachable!("HasMainProcess guarantees main_process is Some"),
        }

        parts.join(" ")
    }

    fn generate_shares(&self) -> Vec<SharedDir> {
        self.shares
            .iter()
            .enumerate()
            .map(|(i, share)| {
                let mode = if share.read_only {
                    MountMode::ReadOnly
                } else {
                    MountMode::ReadWrite
                };
                SharedDir::new(&share.host_path, format!("share{}", i), mode)
            })
            .collect()
    }
}

fn generate_temp_vsock_path(port: u32) -> PathBuf {
    let uuid_short = &uuid::Uuid::new_v4().to_string()[..8];
    PathBuf::from("/tmp").join(format!("capsa-{}-{}.sock", uuid_short, port))
}

#[cfg(test)]
mod tests {
    use super::*;

    mod typestate {
        use super::*;

        #[test]
        fn new_builder_has_no_main_process() {
            let builder = SandboxBuilder::new();
            assert!(builder.main_process.is_none());
        }

        #[test]
        fn run_sets_main_process() {
            let builder = SandboxBuilder::new().run("/bin/sh", &["-c", "echo hi"]);
            assert!(builder.main_process.is_some());
            match builder.main_process.as_ref().unwrap() {
                MainProcess::Run { path, args } => {
                    assert_eq!(path, "/bin/sh");
                    assert_eq!(args, &["-c", "echo hi"]);
                }
                _ => panic!("expected Run"),
            }
        }

        #[test]
        fn oci_sets_main_process() {
            let builder = SandboxBuilder::new().oci("python:3.11", &["python"]);
            assert!(builder.main_process.is_some());
            match builder.main_process.as_ref().unwrap() {
                MainProcess::Oci { image, args } => {
                    assert_eq!(image, "python:3.11");
                    assert_eq!(args, &["python"]);
                }
                _ => panic!("expected Oci"),
            }
        }
    }

    mod builder_methods {
        use super::*;

        #[test]
        fn share_adds_to_shares() {
            let builder = SandboxBuilder::new()
                .share("./src", "/mnt/src", MountMode::ReadOnly)
                .share("./data", "/mnt/data", MountMode::ReadWrite);

            assert_eq!(builder.shares.len(), 2);
            assert_eq!(builder.shares[0].host_path, PathBuf::from("./src"));
            assert_eq!(builder.shares[0].guest_path, "/mnt/src");
            assert!(builder.shares[0].read_only);
            assert_eq!(builder.shares[1].host_path, PathBuf::from("./data"));
            assert!(!builder.shares[1].read_only);
        }

        #[test]
        fn share_works_before_run() {
            let builder = SandboxBuilder::new()
                .share("./src", "/mnt", MountMode::ReadOnly)
                .run("/bin/sh", &[]);

            assert_eq!(builder.shares.len(), 1);
            assert!(builder.main_process.is_some());
        }

        #[test]
        fn share_works_after_run() {
            let builder = SandboxBuilder::new().run("/bin/sh", &[]).share(
                "./src",
                "/mnt",
                MountMode::ReadOnly,
            );

            assert_eq!(builder.shares.len(), 1);
            assert!(builder.main_process.is_some());
        }

        #[test]
        fn cpus_sets_value() {
            let builder = SandboxBuilder::new().cpus(4);
            assert_eq!(builder.resources.cpus, 4);
        }

        #[test]
        fn memory_mb_sets_value() {
            let builder = SandboxBuilder::new().memory_mb(2048);
            assert_eq!(builder.resources.memory_mb, 2048);
        }

        #[test]
        fn no_network_sets_none() {
            let builder = SandboxBuilder::new().no_network();
            assert!(matches!(builder.network, NetworkMode::None));
        }

        #[test]
        fn kernel_override() {
            let builder = SandboxBuilder::new().kernel("/custom/kernel");
            assert_eq!(
                builder.config.kernel_override,
                Some(PathBuf::from("/custom/kernel"))
            );
        }

        #[test]
        fn initrd_override() {
            let builder = SandboxBuilder::new().initrd("/custom/initrd");
            assert_eq!(
                builder.config.initrd_override,
                Some(PathBuf::from("/custom/initrd"))
            );
        }

        #[test]
        fn console_enabled_by_default() {
            let builder = SandboxBuilder::new();
            assert!(builder.console_enabled);
        }
    }

    mod cmdline_generation {
        use super::*;

        #[test]
        fn basic_cmdline() {
            let builder = SandboxBuilder::new().run("/bin/sh", &[]);
            let cmdline = builder.generate_cmdline();

            assert!(cmdline.contains("console=hvc0"));
            assert!(cmdline.contains("panic=-1"));
            assert!(cmdline.contains("quiet"));
            // Path is percent-encoded: / -> %2F
            assert!(cmdline.contains("capsa.run=%2Fbin%2Fsh"));
        }

        #[test]
        fn cmdline_with_shares() {
            let builder = SandboxBuilder::new()
                .share("./src", "/mnt/src", MountMode::ReadOnly)
                .share("./data", "/mnt/data", MountMode::ReadWrite)
                .run("/bin/sh", &[]);
            let cmdline = builder.generate_cmdline();

            assert!(cmdline.contains("capsa.mount=share0:/mnt/src"));
            assert!(cmdline.contains("capsa.mount=share1:/mnt/data"));
        }

        #[test]
        fn cmdline_with_run_args() {
            let builder = SandboxBuilder::new().run("/bin/sh", &["-c", "echo hello"]);
            let cmdline = builder.generate_cmdline();

            // All non-alphanumeric chars are percent-encoded: / -> %2F, - -> %2D, space -> %20
            assert!(cmdline.contains("capsa.run=%2Fbin%2Fsh:%2Dc:echo%20hello"));
        }

        #[test]
        fn shares_use_sequential_tags() {
            let builder = SandboxBuilder::new()
                .share("./a", "/mnt/a", MountMode::ReadOnly)
                .share("./b", "/mnt/b", MountMode::ReadOnly)
                .share("./c", "/mnt/c", MountMode::ReadOnly)
                .run("/bin/sh", &[]);
            let shares = builder.generate_shares();

            assert_eq!(shares.len(), 3);
            assert_eq!(shares[0].guest_path, "share0");
            assert_eq!(shares[1].guest_path, "share1");
            assert_eq!(shares[2].guest_path, "share2");
        }

        #[test]
        fn cmdline_with_oci_basic() {
            let builder = SandboxBuilder::new().oci("python:3.11", &[]);
            let cmdline = builder.generate_cmdline();

            // Image name is percent-encoded: : -> %3A, . -> %2E
            assert!(cmdline.contains("capsa.oci=python%3A3%2E11"));
            assert!(!cmdline.contains("capsa.run="));
        }

        #[test]
        fn cmdline_with_oci_args() {
            let builder = SandboxBuilder::new().oci("python:3.11", &["python", "/app/main.py"]);
            let cmdline = builder.generate_cmdline();

            // All non-alphanumeric chars are percent-encoded
            assert!(cmdline.contains("capsa.oci=python%3A3%2E11:python:%2Fapp%2Fmain%2Epy"));
        }

        #[test]
        fn generate_shares_respects_readonly_flag() {
            let builder = SandboxBuilder::new()
                .share("./ro", "/ro", MountMode::ReadOnly)
                .share("./rw", "/rw", MountMode::ReadWrite)
                .run("/bin/sh", &[]);
            let shares = builder.generate_shares();

            assert_eq!(shares[0].mode, MountMode::ReadOnly);
            assert_eq!(shares[1].mode, MountMode::ReadWrite);
        }
    }
}
