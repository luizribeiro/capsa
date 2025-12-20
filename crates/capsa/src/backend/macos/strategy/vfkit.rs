use super::ExecutionStrategy;
use crate::backend::macos::pty::Pty;
use async_trait::async_trait;
use capsa_core::{
    BackendVmHandle, ConsoleStream, Error, MountMode, NetworkMode, Result, ShareMechanism, VmConfig,
};
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;

pub struct VfkitStrategy {
    vfkit_path: Option<PathBuf>,
}

impl VfkitStrategy {
    pub fn new() -> Self {
        let vfkit_path = which::which("vfkit").ok();
        Self { vfkit_path }
    }

    fn build_args(&self, config: &VmConfig) -> Vec<String> {
        let mut args = vec![
            "--cpus".to_string(),
            config.resources.cpus.to_string(),
            "--memory".to_string(),
            config.resources.memory_mb.to_string(),
            "--kernel".to_string(),
            config.kernel.display().to_string(),
            "--initrd".to_string(),
            config.initrd.display().to_string(),
            "--kernel-cmdline".to_string(),
            config.cmdline.clone(),
        ];

        if let Some(disk) = &config.disk {
            args.push("--device".to_string());
            args.push(format!("virtio-blk,path={}", disk.path.display()));
        }

        match &config.network {
            NetworkMode::None => {}
            NetworkMode::Nat => {
                args.push("--device".to_string());
                args.push("virtio-net,nat".to_string());
            }
        }

        for share in &config.shares {
            let mode = match share.mode {
                MountMode::ReadOnly => "ro",
                MountMode::ReadWrite => "rw",
            };

            let tag = match &share.mechanism {
                ShareMechanism::Auto => share
                    .guest_path
                    .replace('/', "_")
                    .trim_matches('_')
                    .to_string(),
                ShareMechanism::VirtioFs(cfg) => cfg.tag.clone().unwrap_or_else(|| {
                    share
                        .guest_path
                        .replace('/', "_")
                        .trim_matches('_')
                        .to_string()
                }),
                ShareMechanism::Virtio9p(_) => {
                    panic!("virtio-9p is not supported by vfkit backend")
                }
            };

            args.push("--device".to_string());
            args.push(format!(
                "virtio-fs,sharedDir={},mountTag={},{}",
                share.host_path.display(),
                tag,
                mode
            ));
        }

        if config.console_enabled {
            args.push("--device".to_string());
            args.push("virtio-serial,stdio".to_string());
        }

        args
    }
}

impl Default for VfkitStrategy {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionStrategy for VfkitStrategy {
    fn name(&self) -> &'static str {
        "vfkit"
    }

    fn is_available(&self) -> bool {
        self.vfkit_path.is_some()
    }

    async fn start(&self, config: &VmConfig) -> Result<Box<dyn BackendVmHandle>> {
        let vfkit_path = self
            .vfkit_path
            .as_ref()
            .ok_or_else(|| Error::BackendUnavailable {
                name: "vfkit".to_string(),
                reason: "vfkit binary not found in PATH".to_string(),
            })?;

        let args = self.build_args(config);

        tracing::debug!("Starting vfkit with args: {:?}", args);

        let pty = if config.console_enabled {
            Some(
                Pty::new()
                    .map_err(|e| Error::StartFailed(format!("Failed to create PTY: {}", e)))?,
            )
        } else {
            None
        };

        let mut cmd = Command::new(vfkit_path);
        cmd.args(&args);

        if let Some(ref pty) = pty {
            let (stdin, stdout, stderr) = pty.slave_stdio()?;
            cmd.stdin(stdin);
            cmd.stdout(stdout);
            cmd.stderr(stderr);
        } else {
            cmd.stdin(Stdio::null());
            cmd.stdout(Stdio::null());
            cmd.stderr(Stdio::null());
        }

        let child = cmd
            .spawn()
            .map_err(|e| Error::StartFailed(format!("Failed to spawn vfkit: {}", e)))?;

        Ok(Box::new(VfkitVmHandle::new(child, pty)))
    }
}

struct VfkitVmHandle {
    child: Arc<Mutex<Child>>,
    running: AtomicBool,
    pty: Option<Mutex<Option<Pty>>>,
}

impl VfkitVmHandle {
    fn new(child: Child, pty: Option<Pty>) -> Self {
        Self {
            child: Arc::new(Mutex::new(child)),
            running: AtomicBool::new(true),
            pty: pty.map(|p| Mutex::new(Some(p))),
        }
    }
}

#[async_trait]
impl BackendVmHandle for VfkitVmHandle {
    async fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    async fn wait(&self) -> Result<i32> {
        let mut child = self.child.lock().await;
        let status = child.wait().await?;
        self.running.store(false, Ordering::SeqCst);
        Ok(status.code().unwrap_or(-1))
    }

    async fn shutdown(&self) -> Result<()> {
        let child = self.child.lock().await;
        if let Some(id) = child.id() {
            #[cfg(unix)]
            {
                use nix::sys::signal::{self, Signal};
                use nix::unistd::Pid;
                let _ = signal::kill(Pid::from_raw(id as i32), Signal::SIGTERM);
            }
        }
        Ok(())
    }

    async fn kill(&self) -> Result<()> {
        let mut child = self.child.lock().await;
        child.kill().await?;
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn console_stream(&self) -> Result<Option<ConsoleStream>> {
        let Some(pty_mutex) = &self.pty else {
            return Ok(None);
        };

        let mut pty_guard = pty_mutex.lock().await;
        let Some(pty) = pty_guard.take() else {
            return Err(Error::ConsoleNotEnabled);
        };

        let async_master = pty
            .into_async_master()
            .map_err(|e| Error::StartFailed(format!("Failed to create async PTY master: {}", e)))?;

        Ok(Some(Box::new(async_master)))
    }
}
