use super::ExecutionStrategy;
use crate::backend::macos::pty::Pty;
use async_trait::async_trait;
use capsa_apple_vzd_ipc::{PipeTransport, VmConfig, VmHandleId, VmServiceClient};
use capsa_core::{
    BackendVmHandle, ConsoleMode, ConsoleStream, Error, InternalVmConfig, NetworkMode, Result,
};
use std::os::fd::AsRawFd;
use std::process::Stdio;
use std::sync::Arc;
use tarpc::tokio_serde::formats::Bincode;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

pub struct SubprocessStrategy;

impl SubprocessStrategy {
    pub fn new() -> Self {
        Self
    }

    fn find_vzd_binary() -> Option<std::path::PathBuf> {
        if let Ok(path) = std::env::var("CAPSA_VZD_PATH") {
            let path = std::path::PathBuf::from(path);
            if path.exists() {
                return Some(path);
            }
        }

        if let Some(bundled) = option_env!("CAPSA_VZD_BUNDLED") {
            let path = std::path::PathBuf::from(bundled);
            if path.exists() {
                return Some(path);
            }
        }

        if let Ok(exe) = std::env::current_exe() {
            let dir = exe.parent()?;
            let vzd_path = dir.join("capsa-apple-vzd");
            if vzd_path.exists() {
                return Some(vzd_path);
            }
        }

        which::which("capsa-apple-vzd").ok()
    }
}

impl Default for SubprocessStrategy {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ExecutionStrategy for SubprocessStrategy {
    fn name(&self) -> &'static str {
        "subprocess-virtualization"
    }

    fn is_available(&self) -> bool {
        Self::find_vzd_binary().is_some()
    }

    async fn start(&self, config: &InternalVmConfig) -> Result<Box<dyn BackendVmHandle>> {
        let vzd_path = Self::find_vzd_binary().ok_or(Error::NoBackendAvailable)?;

        let console_enabled = config.console != ConsoleMode::Disabled;
        let pty = if console_enabled {
            Some(
                Pty::new()
                    .map_err(|e| Error::StartFailed(format!("Failed to create PTY: {}", e)))?,
            )
        } else {
            None
        };

        let mut cmd = Command::new(&vzd_path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        if let Some(ref pty) = pty {
            let slave_fd = pty.slave.as_raw_fd();
            // SAFETY: pre_exec runs after fork() but before exec() in the child process.
            // At this point, the child has a copy of all file descriptors from the parent.
            // Requirements:
            // 1. slave_fd is valid because pty.slave is kept alive until spawn() completes
            // 2. dup2 and close are async-signal-safe and appropriate for use in pre_exec
            // 3. We duplicate to fd 3 which the vzd subprocess expects for console I/O
            // 4. After dup2, we close the original slave_fd to avoid leaking it in the child
            // 5. The parent's copy of slave_fd will be closed when Pty is dropped
            unsafe {
                cmd.pre_exec(move || {
                    if slave_fd != 3 {
                        nix::unistd::dup2(slave_fd, 3).map_err(std::io::Error::other)?;
                        nix::unistd::close(slave_fd).map_err(std::io::Error::other)?;
                    }
                    Ok(())
                });
            }
        }

        let mut child = cmd
            .spawn()
            .map_err(|e| Error::StartFailed(format!("Failed to spawn vzd: {}", e)))?;

        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();

        let transport = PipeTransport::new(stdout, stdin);
        let framed = Framed::new(transport, LengthDelimitedCodec::new());
        let serde_transport = tarpc::serde_transport::new(framed, Bincode::default());

        let client =
            VmServiceClient::new(tarpc::client::Config::default(), serde_transport).spawn();

        let rpc_config = VmConfig {
            kernel: config.kernel.clone(),
            initrd: config.initrd.clone(),
            disk: config
                .disk
                .as_ref()
                .map(|d| capsa_apple_vzd_ipc::DiskConfig {
                    path: d.path.clone(),
                    read_only: false,
                }),
            cmdline: config.cmdline.clone(),
            cpus: config.resources.cpus,
            memory_mb: config.resources.memory_mb,
            shares: config
                .shares
                .iter()
                .map(|s| capsa_apple_vzd_ipc::SharedDirConfig {
                    host_path: s.host_path.clone(),
                    guest_path: s.guest_path.clone(),
                    read_only: matches!(s.mode, capsa_core::MountMode::ReadOnly),
                })
                .collect(),
            network: match config.network {
                NetworkMode::None => capsa_apple_vzd_ipc::NetworkMode::None,
                NetworkMode::Nat => capsa_apple_vzd_ipc::NetworkMode::Nat,
            },
            console_enabled,
        };

        let handle_id = client
            .start(tarpc::context::current(), rpc_config, None)
            .await
            .map_err(|e| Error::StartFailed(format!("RPC call to start VM failed: {}", e)))?
            .map_err(|e| Error::StartFailed(format!("VM subprocess failed to start: {}", e)))?;

        Ok(Box::new(SubprocessVmHandle {
            handle_id,
            client: Arc::new(client),
            _child: Arc::new(Mutex::new(child)),
            pty: pty.map(|p| Mutex::new(Some(p))),
        }))
    }
}

struct SubprocessVmHandle {
    handle_id: VmHandleId,
    client: Arc<VmServiceClient>,
    _child: Arc<Mutex<tokio::process::Child>>,
    pty: Option<Mutex<Option<Pty>>>,
}

#[async_trait]
impl BackendVmHandle for SubprocessVmHandle {
    async fn is_running(&self) -> bool {
        self.client
            .is_running(tarpc::context::current(), self.handle_id)
            .await
            .ok()
            .and_then(|r| r.ok())
            .unwrap_or(false)
    }

    async fn wait(&self) -> Result<i32> {
        self.client
            .wait(tarpc::context::current(), self.handle_id)
            .await
            .map_err(|e| Error::Hypervisor(format!("RPC call to wait for VM failed: {}", e)))?
            .map_err(|e| Error::Hypervisor(format!("VM wait failed: {}", e)))
    }

    async fn shutdown(&self) -> Result<()> {
        self.client
            .shutdown(tarpc::context::current(), self.handle_id)
            .await
            .map_err(|e| Error::Hypervisor(format!("RPC call to shutdown VM failed: {}", e)))?
            .map_err(|e| Error::Hypervisor(format!("VM shutdown failed: {}", e)))
    }

    async fn kill(&self) -> Result<()> {
        self.client
            .kill(tarpc::context::current(), self.handle_id)
            .await
            .map_err(|e| Error::Hypervisor(format!("RPC call to kill VM failed: {}", e)))?
            .map_err(|e| Error::Hypervisor(format!("VM kill failed: {}", e)))
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

impl Drop for SubprocessVmHandle {
    fn drop(&mut self) {
        let client = self.client.clone();
        let handle_id = self.handle_id;
        let runtime = tokio::runtime::Handle::current();

        // Spawn a thread and block on it to ensure release completes before drop returns.
        // This ensures deterministic cleanup even if the process exits immediately after drop.
        // We use thread::scope because block_on cannot be called from within an async context.
        let _ = std::thread::scope(|s| {
            s.spawn(|| {
                let _ = runtime.block_on(async {
                    tokio::time::timeout(
                        std::time::Duration::from_secs(5),
                        client.release(tarpc::context::current(), handle_id),
                    )
                    .await
                });
            })
            .join()
        });
    }
}
