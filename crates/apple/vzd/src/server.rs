use crate::console;
use capsa_apple_vz::NativeVirtualizationBackend;
use capsa_apple_vzd_ipc::{RpcResult, VmConfig, VmHandleId, VmService};
use capsa_core::{BackendVmHandle, HypervisorBackend};
use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use tarpc::context::Context;
use tokio::sync::RwLock;

#[derive(Clone)]
pub struct VzdServer {
    handles: Arc<RwLock<HashMap<VmHandleId, VmHandle>>>,
    next_id: Arc<AtomicU64>,
}

struct VmHandle {
    handle: Box<dyn BackendVmHandle>,
}

impl Default for VzdServer {
    fn default() -> Self {
        Self::new()
    }
}

impl VzdServer {
    pub fn new() -> Self {
        Self {
            handles: Arc::new(RwLock::new(HashMap::new())),
            next_id: Arc::new(AtomicU64::new(1)),
        }
    }

    pub fn next_handle_id(&self) -> VmHandleId {
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
        let backend = NativeVirtualizationBackend::new();
        let handle: Box<dyn BackendVmHandle> =
            backend.start(&config).await.map_err(|e| e.to_string())?;

        if config.console_enabled
            && let Ok(Some(stream)) = handle.console_stream().await
            && let Some(fd3) = console::try_get_fd3()
        {
            console::spawn_proxy(fd3, stream);
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
