//! VM handle for managing running virtual machines.
//!
//! The [`VmHandle`] is your primary interface for controlling a running VM's
//! lifecycle. Use it to:
//!
//! - Monitor VM status via [`VmHandle::status`]
//! - Gracefully stop or forcefully kill the VM
//! - Wait for the VM to exit
//! - Access the serial console via [`VmHandle::console`]
//!
//! # Lifecycle
//!
//! VMs created via [`Capsa::linux`](crate::Capsa::linux) start in the `Running`
//! state. Use [`stop`](VmHandle::stop) for graceful shutdown or
//! [`kill`](VmHandle::kill) for immediate termination.

use crate::console::VmConsole;
use capsa_core::{BackendVmHandle, Error, GuestOs, ResourceConfig, Result};
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

/// Current status of a virtual machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VmStatus {
    /// VM has been created but not yet started.
    Created,
    /// VM is in the process of starting.
    Starting,
    /// VM is running.
    Running,
    /// VM is in the process of stopping.
    Stopping,
    /// VM has stopped.
    Stopped {
        /// Exit code from the VM, if available.
        exit_code: Option<i32>,
    },
    /// VM has failed.
    Failed {
        /// Error message describing the failure.
        message: String,
    },
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

/// Handle to a running virtual machine.
///
/// Provides methods for controlling the VM lifecycle (start, stop, kill),
/// monitoring status, and accessing the console.
pub struct VmHandle {
    backend_handle: Arc<Box<dyn BackendVmHandle>>,
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

    /// Starts the VM if it's not already running.
    pub async fn start(&self) -> Result<()> {
        let current = self.status.load(Ordering::SeqCst);
        if current == STATUS_RUNNING {
            return Err(Error::AlreadyRunning);
        }

        self.status.store(STATUS_STARTING, Ordering::SeqCst);
        self.status.store(STATUS_RUNNING, Ordering::SeqCst);
        Ok(())
    }

    /// Gracefully stops the VM with a 30-second timeout.
    ///
    /// Sends a shutdown request to the guest. If the guest doesn't stop
    /// within the timeout, it will be forcefully killed.
    pub async fn stop(&self) -> Result<()> {
        self.stop_with_timeout(Duration::from_secs(30)).await
    }

    /// Gracefully stops the VM with a custom timeout.
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

    /// Forcefully terminates the VM immediately.
    pub async fn kill(&self) -> Result<()> {
        self.backend_handle.kill().await?;
        self.status.store(STATUS_STOPPED, Ordering::SeqCst);
        Ok(())
    }

    /// Returns the current status of the VM.
    pub fn status(&self) -> VmStatus {
        let status = self.status.load(Ordering::SeqCst);
        let exit_code = *self.exit_code.lock().unwrap();
        let error_msg = self.error_message.lock().unwrap().clone();
        VmStatus::from_atomic(status, exit_code, error_msg)
    }

    /// Waits for the VM to exit and returns its final status.
    pub async fn wait(&self) -> Result<VmStatus> {
        let code = self.backend_handle.wait().await?;
        *self.exit_code.lock().unwrap() = Some(code);
        self.status.store(STATUS_STOPPED, Ordering::SeqCst);
        Ok(self.status())
    }

    /// Waits for the VM to exit with a timeout.
    ///
    /// Returns `None` if the timeout expires before the VM exits.
    pub async fn wait_timeout(&self, duration: Duration) -> Result<Option<VmStatus>> {
        match timeout(duration, self.wait()).await {
            Ok(result) => result.map(Some),
            Err(_) => Ok(None),
        }
    }

    /// Gets the console interface for interacting with the VM.
    ///
    /// Requires that the VM was built with `.console_enabled()`.
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

    /// Returns the guest operating system type.
    pub fn guest_os(&self) -> GuestOs {
        self.guest_os
    }

    /// Returns the resource configuration (CPUs, memory) of the VM.
    pub fn resources(&self) -> &ResourceConfig {
        &self.resources
    }
}
