use capsa_apple_vzd_ipc::{PipeTransport, RpcResult, VmConfig, VmHandleId, VmService};
use capsa_backend_native::NativeVirtualizationBackend;
use capsa_core::{
    AsyncOwnedFd, BackendVmHandle, ConsoleMode, ConsoleStream, DiskImage, HypervisorBackend,
    InternalVmConfig, MountMode, NetworkMode, ResourceConfig, SharedDir,
};
use futures::prelude::*;
use std::collections::HashMap;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tarpc::context::Context;
use tarpc::server::{BaseChannel, Channel};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::RwLock;
use tokio_util::codec::{Framed, LengthDelimitedCodec};
use tracing::info;

#[derive(Clone)]
struct VzdServer {
    handles: Arc<RwLock<HashMap<VmHandleId, VmHandle>>>,
    next_id: Arc<AtomicU64>,
}

struct VmHandle {
    handle: Box<dyn BackendVmHandle>,
}

impl VzdServer {
    fn new() -> Self {
        Self {
            handles: Arc::new(RwLock::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    fn next_handle_id(&self) -> VmHandleId {
        VmHandleId(self.next_id.fetch_add(1, Ordering::SeqCst))
    }
}

impl VmService for VzdServer {
    async fn is_available(self, _: Context) -> bool {
        NativeVirtualizationBackend::new().is_available()
    }

    async fn start(
        self,
        _: Context,
        config: VmConfig,
        _console_socket_path: Option<String>,
    ) -> RpcResult<VmHandleId> {
        let internal_config = convert_config(&config);

        let backend = NativeVirtualizationBackend::new();
        let handle: Box<dyn BackendVmHandle> = backend
            .start(&internal_config)
            .await
            .map_err(|e| e.to_string())?;

        if config.console_enabled
            && let Ok(Some(console)) = handle.console_stream().await
            && let Some(fd3) = try_get_fd3()
        {
            spawn_console_proxy(fd3, console);
        }

        let handle_id = self.next_handle_id();
        self.handles
            .write()
            .await
            .insert(handle_id, VmHandle { handle });
        Ok(handle_id)
    }

    async fn is_running(self, _: Context, handle: VmHandleId) -> RpcResult<bool> {
        let handles = self.handles.read().await;
        let vm = handles.get(&handle).ok_or("Handle not found")?;
        Ok(vm.handle.is_running().await)
    }

    async fn wait(self, _: Context, handle: VmHandleId) -> RpcResult<i32> {
        let handles = self.handles.read().await;
        let vm = handles.get(&handle).ok_or("Handle not found")?;
        vm.handle.wait().await.map_err(|e| e.to_string())
    }

    async fn shutdown(self, _: Context, handle: VmHandleId) -> RpcResult<()> {
        let handles = self.handles.read().await;
        let vm = handles.get(&handle).ok_or("Handle not found")?;
        vm.handle.shutdown().await.map_err(|e| e.to_string())
    }

    async fn kill(self, _: Context, handle: VmHandleId) -> RpcResult<()> {
        let handles = self.handles.read().await;
        let vm = handles.get(&handle).ok_or("Handle not found")?;
        vm.handle.kill().await.map_err(|e| e.to_string())
    }

    async fn release(self, _: Context, handle: VmHandleId) -> RpcResult<()> {
        self.handles.write().await.remove(&handle);
        Ok(())
    }
}

fn convert_config(config: &VmConfig) -> InternalVmConfig {
    InternalVmConfig {
        kernel: config.kernel.clone(),
        initrd: config.initrd.clone(),
        disk: config.disk.as_ref().map(|d| DiskImage::new(&d.path)),
        cmdline: config.cmdline.clone(),
        resources: ResourceConfig {
            cpus: config.cpus,
            memory_mb: config.memory_mb,
        },
        shares: config
            .shares
            .iter()
            .map(|s| {
                SharedDir::new(
                    &s.host_path,
                    &s.guest_path,
                    if s.read_only {
                        MountMode::ReadOnly
                    } else {
                        MountMode::ReadWrite
                    },
                )
            })
            .collect(),
        network: match config.network {
            capsa_apple_vzd_ipc::NetworkMode::None => NetworkMode::None,
            capsa_apple_vzd_ipc::NetworkMode::Nat => NetworkMode::Nat,
        },
        console: if config.console_enabled {
            ConsoleMode::Enabled
        } else {
            ConsoleMode::Disabled
        },
    }
}

fn try_get_fd3() -> Option<OwnedFd> {
    use nix::fcntl::{FcntlArg, fcntl};

    let fd = 3;
    match fcntl(fd, FcntlArg::F_GETFD) {
        Ok(_) => Some(unsafe { OwnedFd::from_raw_fd(fd) }),
        Err(_) => None,
    }
}

fn spawn_console_proxy(fd3: OwnedFd, console: ConsoleStream) {
    use nix::fcntl::{FcntlArg, OFlag, fcntl};

    let flags = fcntl(fd3.as_raw_fd(), FcntlArg::F_GETFL).unwrap_or(0);
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    let _ = fcntl(fd3.as_raw_fd(), FcntlArg::F_SETFL(flags));

    tokio::spawn(async move {
        let async_fd3 = match AsyncOwnedFd::new(fd3) {
            Ok(fd) => fd,
            Err(e) => {
                tracing::error!("Failed to create async fd3: {}", e);
                return;
            }
        };

        let (mut console_read, mut console_write) = tokio::io::split(console);
        let (mut fd3_read, mut fd3_write) = tokio::io::split(async_fd3);

        let console_to_fd3 = async {
            let mut buf = [0u8; 4096];
            loop {
                match console_read.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if fd3_write.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };

        let fd3_to_console = async {
            let mut buf = [0u8; 4096];
            loop {
                match fd3_read.read(&mut buf).await {
                    Ok(0) => break,
                    Ok(n) => {
                        if console_write.write_all(&buf[..n]).await.is_err() {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        };

        tokio::select! {
            _ = console_to_fd3 => {}
            _ = fd3_to_console => {}
        }
    });
}

#[apple_main::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .init();

    info!("capsa-apple-vzd starting");

    let server = VzdServer::new();

    let stdin = tokio::io::stdin();
    let stdout = tokio::io::stdout();

    let transport = PipeTransport::new(stdin, stdout);
    let framed = Framed::new(transport, LengthDelimitedCodec::new());

    let transport = tarpc::serde_transport::new(framed, tokio_serde::formats::Bincode::default());

    BaseChannel::with_defaults(transport)
        .execute(server.serve())
        .for_each(|response| async move {
            tokio::spawn(response);
        })
        .await;

    info!("capsa-apple-vzd shutting down");
}

#[cfg(test)]
mod tests {
    use super::*;

    mod vzd_server {
        use super::*;

        #[test]
        fn new_creates_empty_server() {
            let server = VzdServer::new();
            assert_eq!(server.next_id.load(Ordering::SeqCst), 1);
        }

        #[test]
        fn next_handle_id_increments() {
            let server = VzdServer::new();

            let id1 = server.next_handle_id();
            let id2 = server.next_handle_id();
            let id3 = server.next_handle_id();

            assert_eq!(id1.0, 1);
            assert_eq!(id2.0, 2);
            assert_eq!(id3.0, 3);
        }

        #[test]
        fn next_handle_id_is_thread_safe() {
            let server = VzdServer::new();
            let server1 = server.clone();
            let server2 = server.clone();

            let handle1 = std::thread::spawn(move || {
                (0..100)
                    .map(|_| server1.next_handle_id().0)
                    .collect::<Vec<_>>()
            });
            let handle2 = std::thread::spawn(move || {
                (0..100)
                    .map(|_| server2.next_handle_id().0)
                    .collect::<Vec<_>>()
            });

            let ids1 = handle1.join().unwrap();
            let ids2 = handle2.join().unwrap();

            let mut all_ids: Vec<_> = ids1.into_iter().chain(ids2).collect();
            all_ids.sort();
            all_ids.dedup();
            assert_eq!(all_ids.len(), 200);
        }
    }

    mod convert_config {
        use super::*;
        use capsa_apple_vzd_ipc::{DiskConfig, SharedDirConfig};

        fn sample_config() -> VmConfig {
            VmConfig {
                kernel: "/path/to/kernel".into(),
                initrd: "/path/to/initrd".into(),
                disk: Some(DiskConfig {
                    path: "/path/to/disk.raw".into(),
                    read_only: false,
                }),
                cmdline: "console=hvc0 root=/dev/vda".to_string(),
                cpus: 4,
                memory_mb: 2048,
                shares: vec![SharedDirConfig {
                    host_path: "/host/path".into(),
                    guest_path: "/guest/path".to_string(),
                    read_only: true,
                }],
                network: capsa_apple_vzd_ipc::NetworkMode::Nat,
                console_enabled: true,
            }
        }

        #[test]
        fn converts_basic_fields() {
            let config = sample_config();
            let internal = convert_config(&config);

            assert_eq!(internal.kernel, config.kernel);
            assert_eq!(internal.initrd, config.initrd);
            assert_eq!(internal.cmdline, config.cmdline);
            assert_eq!(internal.resources.cpus, config.cpus);
            assert_eq!(internal.resources.memory_mb, config.memory_mb);
        }

        #[test]
        fn converts_disk_config() {
            let config = sample_config();
            let internal = convert_config(&config);

            assert!(internal.disk.is_some());
            assert_eq!(
                internal.disk.as_ref().unwrap().path,
                config.disk.as_ref().unwrap().path
            );
        }

        #[test]
        fn converts_none_disk() {
            let mut config = sample_config();
            config.disk = None;
            let internal = convert_config(&config);

            assert!(internal.disk.is_none());
        }

        #[test]
        fn converts_shares_with_read_only() {
            let config = sample_config();
            let internal = convert_config(&config);

            assert_eq!(internal.shares.len(), 1);
            assert_eq!(internal.shares[0].host_path, config.shares[0].host_path);
            assert_eq!(internal.shares[0].guest_path, config.shares[0].guest_path);
            assert_eq!(internal.shares[0].mode, MountMode::ReadOnly);
        }

        #[test]
        fn converts_shares_with_read_write() {
            let mut config = sample_config();
            config.shares[0].read_only = false;
            let internal = convert_config(&config);

            assert_eq!(internal.shares[0].mode, MountMode::ReadWrite);
        }

        #[test]
        fn converts_network_none() {
            let mut config = sample_config();
            config.network = capsa_apple_vzd_ipc::NetworkMode::None;
            let internal = convert_config(&config);

            assert_eq!(internal.network, NetworkMode::None);
        }

        #[test]
        fn converts_network_nat() {
            let config = sample_config();
            let internal = convert_config(&config);

            assert_eq!(internal.network, NetworkMode::Nat);
        }

        #[test]
        fn converts_console_enabled() {
            let config = sample_config();
            let internal = convert_config(&config);

            assert_eq!(internal.console, ConsoleMode::Enabled);
        }

        #[test]
        fn converts_console_disabled() {
            let mut config = sample_config();
            config.console_enabled = false;
            let internal = convert_config(&config);

            assert_eq!(internal.console, ConsoleMode::Disabled);
        }
    }
}
