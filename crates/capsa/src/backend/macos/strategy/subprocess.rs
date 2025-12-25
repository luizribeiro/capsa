use super::ExecutionStrategy;
use crate::backend::macos::pty::Pty;
use crate::cluster::NetworkCluster;
use async_trait::async_trait;
use capsa_apple_vzd_ipc::{PipeTransport, VmHandleId, VmServiceClient};
use capsa_core::{BackendVmHandle, ConsoleStream, Error, NetworkMode, Result, VmConfig};
use capsa_net::SwitchPort;
use std::os::fd::{AsRawFd, OwnedFd};
use std::process::Stdio;
use std::sync::Arc;
use tarpc::tokio_serde::formats::Bincode;
use tokio::process::Command;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tokio_util::codec::{Framed, LengthDelimitedCodec};

struct ClusterPortInfo {
    guest_fd: OwnedFd,
    host_fd: OwnedFd,
    switch_port: SwitchPort,
}

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

    async fn start(&self, config: &VmConfig) -> Result<Box<dyn BackendVmHandle>> {
        let vzd_path = Self::find_vzd_binary().ok_or(Error::NoBackendAvailable)?;

        let pty = if config.console_enabled {
            Some(
                Pty::new()
                    .map_err(|e| Error::StartFailed(format!("Failed to create PTY: {}", e)))?,
            )
        } else {
            None
        };

        // For Cluster mode, create the cluster port and prepare to pass fd
        let cluster_port: Option<ClusterPortInfo> =
            if let NetworkMode::Cluster(ref cluster_config) = config.network {
                let cluster = NetworkCluster::get_or_create(&cluster_config.cluster_name);
                let port = cluster.create_port().await?;
                Some(ClusterPortInfo {
                    guest_fd: port.guest_fd,
                    host_fd: port.host_fd,
                    switch_port: port.switch_port,
                })
            } else {
                None
            };

        let mut cmd = Command::new(&vzd_path);
        cmd.stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit());

        // Prepare fds for pre_exec
        let pty_slave_fd = pty.as_ref().map(|p| p.slave.as_raw_fd());
        let network_guest_fd = cluster_port.as_ref().map(|p| p.guest_fd.as_raw_fd());

        // SAFETY: pre_exec runs after fork() but before exec() in the child process.
        // At this point, the child has a copy of all file descriptors from the parent.
        // Requirements:
        // 1. slave_fd and network_fd are valid because they're kept alive until spawn() completes
        // 2. dup2 and close are async-signal-safe and appropriate for use in pre_exec
        // 3. We duplicate to fd 3 (console) and fd 4 (network) which vzd expects
        // 4. After dup2, we close the original fds to avoid leaking them in the child
        // 5. The parent's copies will be closed when Pty/ClusterPort are dropped
        unsafe {
            cmd.pre_exec(move || {
                if let Some(slave_fd) = pty_slave_fd {
                    if slave_fd != 3 {
                        nix::unistd::dup2(slave_fd, 3).map_err(std::io::Error::other)?;
                        nix::unistd::close(slave_fd).map_err(std::io::Error::other)?;
                    }
                }
                if let Some(net_fd) = network_guest_fd {
                    if net_fd != 4 {
                        nix::unistd::dup2(net_fd, 4).map_err(std::io::Error::other)?;
                        nix::unistd::close(net_fd).map_err(std::io::Error::other)?;
                    }
                }
                Ok(())
            });
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

        let handle_id = client
            .start(tarpc::context::current(), config.clone(), None)
            .await
            .map_err(|e| Error::StartFailed(format!("RPC call to start VM failed: {}", e)))?
            .map_err(|e| Error::StartFailed(format!("VM subprocess failed to start: {}", e)))?;

        // Spawn the bridge task for Cluster mode
        let bridge_task = if let Some(port) = cluster_port {
            use capsa_net::bridge_to_switch;

            Some(tokio::spawn(async move {
                if let Err(e) = bridge_to_switch(port.host_fd, port.switch_port).await {
                    tracing::error!(error = %e, "Cluster bridge error");
                }
            }))
        } else {
            None
        };

        Ok(Box::new(SubprocessVmHandle {
            handle_id,
            client: Arc::new(client),
            _child: Arc::new(Mutex::new(child)),
            pty: pty.map(|p| Mutex::new(Some(p))),
            _bridge_task: bridge_task,
        }))
    }
}

struct SubprocessVmHandle {
    handle_id: VmHandleId,
    client: Arc<VmServiceClient>,
    _child: Arc<Mutex<tokio::process::Child>>,
    pty: Option<Mutex<Option<Pty>>>,
    #[allow(dead_code)]
    _bridge_task: Option<JoinHandle<()>>,
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
