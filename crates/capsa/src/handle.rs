use crate::backend::BackendVmHandle;
use crate::console::VmConsole;
use crate::error::{Error, Result};
use crate::types::{GuestOs, ResourceConfig};
use std::sync::Arc;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::Duration;
use tokio::time::timeout;

const STATUS_CREATED: u8 = 0;
const STATUS_STARTING: u8 = 1;
const STATUS_RUNNING: u8 = 2;
const STATUS_STOPPING: u8 = 3;
const STATUS_STOPPED: u8 = 4;
const STATUS_FAILED: u8 = 5;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmStatus {
    Created,
    Starting,
    Running,
    Stopping,
    Stopped { exit_code: Option<i32> },
    Failed { message: String },
}

impl VmStatus {
    fn from_atomic(val: u8, exit_code: Option<i32>, error_msg: Option<String>) -> Self {
        match val {
            STATUS_CREATED => VmStatus::Created,
            STATUS_STARTING => VmStatus::Starting,
            STATUS_RUNNING => VmStatus::Running,
            STATUS_STOPPING => VmStatus::Stopping,
            STATUS_STOPPED => VmStatus::Stopped { exit_code },
            STATUS_FAILED => VmStatus::Failed {
                message: error_msg.unwrap_or_else(|| "Unknown error".to_string()),
            },
            _ => VmStatus::Failed {
                message: "Invalid status".to_string(),
            },
        }
    }
}

pub struct VmHandle {
    backend_handle: Arc<Box<dyn BackendVmHandle>>,
    // TODO: exit code and error message already have mutexes... is it really worth it to
    // keep this as AtomicU8?
    status: AtomicU8,
    exit_code: std::sync::Mutex<Option<i32>>,
    error_message: std::sync::Mutex<Option<String>>,
    guest_os: GuestOs,
    resources: ResourceConfig,
}

impl VmHandle {
    pub(crate) fn new(
        backend_handle: Box<dyn BackendVmHandle>,
        guest_os: GuestOs,
        resources: ResourceConfig,
    ) -> Self {
        Self {
            backend_handle: Arc::new(backend_handle),
            status: AtomicU8::new(STATUS_RUNNING),
            exit_code: std::sync::Mutex::new(None),
            error_message: std::sync::Mutex::new(None),
            guest_os,
            resources,
        }
    }

    pub async fn start(&self) -> Result<()> {
        let current = self.status.load(Ordering::SeqCst);
        if current == STATUS_RUNNING {
            return Err(Error::AlreadyRunning);
        }

        self.status.store(STATUS_STARTING, Ordering::SeqCst);
        self.status.store(STATUS_RUNNING, Ordering::SeqCst);
        Ok(())
    }

    pub async fn stop(&self) -> Result<()> {
        self.stop_with_timeout(Duration::from_secs(30)).await
    }

    pub async fn stop_with_timeout(&self, grace_period: Duration) -> Result<()> {
        let current = self.status.load(Ordering::SeqCst);
        if current != STATUS_RUNNING {
            return Err(Error::NotRunning);
        }

        self.status.store(STATUS_STOPPING, Ordering::SeqCst);

        self.backend_handle.shutdown().await?;

        match timeout(grace_period, self.backend_handle.wait()).await {
            Ok(Ok(code)) => {
                *self.exit_code.lock().unwrap() = Some(code);
                self.status.store(STATUS_STOPPED, Ordering::SeqCst);
            }
            Ok(Err(e)) => {
                *self.error_message.lock().unwrap() = Some(e.to_string());
                self.status.store(STATUS_FAILED, Ordering::SeqCst);
                return Err(e);
            }
            Err(_) => {
                self.backend_handle.kill().await?;
                self.status.store(STATUS_STOPPED, Ordering::SeqCst);
            }
        }

        Ok(())
    }

    pub async fn kill(&self) -> Result<()> {
        self.backend_handle.kill().await?;
        self.status.store(STATUS_STOPPED, Ordering::SeqCst);
        Ok(())
    }

    pub fn status(&self) -> VmStatus {
        let status = self.status.load(Ordering::SeqCst);
        let exit_code = *self.exit_code.lock().unwrap();
        let error_msg = self.error_message.lock().unwrap().clone();
        VmStatus::from_atomic(status, exit_code, error_msg)
    }

    pub async fn wait(&self) -> Result<VmStatus> {
        let code = self.backend_handle.wait().await?;
        *self.exit_code.lock().unwrap() = Some(code);
        self.status.store(STATUS_STOPPED, Ordering::SeqCst);
        Ok(self.status())
    }

    pub async fn wait_timeout(&self, duration: Duration) -> Result<Option<VmStatus>> {
        match timeout(duration, self.wait()).await {
            Ok(result) => result.map(Some),
            Err(_) => Ok(None),
        }
    }

    pub async fn console(&self) -> Result<VmConsole> {
        let stream = self
            .backend_handle
            .console_stream()
            .await?
            .ok_or(Error::ConsoleNotEnabled)?;
        Ok(VmConsole::new(stream))
    }

    // TODO: add support for obtaining an agent via vsock which would give better ergonomics
    // for running commands and other things within the VM (than reading/writing from the
    // serial console manually). this might require a VmBuilder that has enabled agent support
    // (and a guest with the agent running in it, of course)

    pub fn guest_os(&self) -> GuestOs {
        self.guest_os
    }

    pub fn resources(&self) -> &ResourceConfig {
        &self.resources
    }
}
