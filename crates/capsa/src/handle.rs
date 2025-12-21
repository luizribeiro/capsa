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
//! VMs created via [`Capsa::vm`](crate::Capsa::vm) start in the `Running`
//! state. Use [`stop`](VmHandle::stop) for graceful shutdown or
//! [`kill`](VmHandle::kill) for immediate termination.

use crate::console::VmConsole;
use crate::vsock::VsockSocket;
use capsa_core::{BackendVmHandle, Error, GuestOs, ResourceConfig, Result, VsockConfig};
use std::collections::HashMap;
use std::path::PathBuf;
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
    /// Auto-generated temp files to clean up when VM stops (e.g., EFI variable store, vsock sockets)
    temp_files: Vec<PathBuf>,
    /// Vsock port to socket mappings for easy access
    vsock_sockets: HashMap<u32, VsockSocket>,
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
            temp_files: Vec::new(),
            vsock_sockets: HashMap::new(),
        }
    }

    pub(crate) fn with_temp_file(mut self, path: PathBuf) -> Self {
        self.temp_files.push(path);
        self
    }

    pub(crate) fn with_temp_files(mut self, paths: impl IntoIterator<Item = PathBuf>) -> Self {
        self.temp_files.extend(paths);
        self
    }

    pub(crate) fn with_vsock_config(mut self, config: &VsockConfig) -> Self {
        for port_config in &config.ports {
            self.vsock_sockets.insert(
                port_config.port(),
                VsockSocket::new(port_config.port(), port_config.socket_path().to_path_buf()),
            );
        }
        self
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
                self.cleanup_temp_files();
            }
            Ok(Err(e)) => {
                *self.error_message.lock().unwrap() = Some(e.to_string());
                self.status.store(STATUS_FAILED, Ordering::SeqCst);
                self.cleanup_temp_files();
                return Err(e);
            }
            Err(_) => {
                self.backend_handle.kill().await?;
                self.status.store(STATUS_STOPPED, Ordering::SeqCst);
                self.cleanup_temp_files();
            }
        }

        Ok(())
    }

    /// Forcefully terminates the VM immediately.
    pub async fn kill(&self) -> Result<()> {
        self.backend_handle.kill().await?;
        self.status.store(STATUS_STOPPED, Ordering::SeqCst);
        self.cleanup_temp_files();
        Ok(())
    }

    fn cleanup_temp_files(&self) {
        for path in &self.temp_files {
            if let Err(e) = std::fs::remove_file(path) {
                if e.kind() != std::io::ErrorKind::NotFound {
                    tracing::warn!("Failed to clean up temp file {:?}: {}", path, e);
                }
            }
        }
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
        self.cleanup_temp_files();
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

    /// Returns the vsock socket for a port, if configured.
    ///
    /// Use [`VsockSocket::connect`] to establish a connection to a
    /// service running in the guest.
    ///
    /// # Example
    ///
    /// ```rust,no_run
    /// # use capsa::{Capsa, LinuxDirectBootConfig};
    /// # use tokio::io::{AsyncReadExt, AsyncWriteExt};
    /// # async fn example() -> capsa::Result<()> {
    /// let config = LinuxDirectBootConfig::new("./kernel", "./initrd");
    /// let vm = Capsa::vm(config)
    ///     .vsock_listen(1024)
    ///     .build().await?;
    ///
    /// // Connect to the guest service
    /// if let Some(socket) = vm.vsock_socket(1024) {
    ///     let mut stream = socket.connect().await?;
    ///     stream.write_all(b"hello").await?;
    /// }
    /// # Ok(())
    /// # }
    /// ```
    pub fn vsock_socket(&self, port: u32) -> Option<&VsockSocket> {
        self.vsock_sockets.get(&port)
    }

    /// Returns all configured vsock sockets.
    pub fn vsock_sockets(&self) -> &HashMap<u32, VsockSocket> {
        &self.vsock_sockets
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;
    use capsa_core::ConsoleStream;
    use tempfile::NamedTempFile;

    #[test]
    fn cleanup_temp_files_deletes_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();
        temp_file.keep().unwrap();

        assert!(path.exists(), "Temp file should exist before cleanup");

        let handle = create_test_handle().with_temp_file(path.clone());
        handle.cleanup_temp_files();

        assert!(!path.exists(), "Temp file should be deleted after cleanup");
    }

    #[test]
    fn cleanup_temp_files_ignores_missing_file() {
        let path = std::env::temp_dir().join("nonexistent-capsa-test-file.efivarstore");
        assert!(!path.exists());

        let handle = create_test_handle().with_temp_file(path);
        handle.cleanup_temp_files();
    }

    #[test]
    fn cleanup_temp_files_does_nothing_without_temp_file() {
        let handle = create_test_handle();
        handle.cleanup_temp_files();
    }

    #[test]
    fn with_temp_file_adds_path() {
        let path = PathBuf::from("/test/path.efivarstore");
        let handle = create_test_handle().with_temp_file(path.clone());
        assert_eq!(handle.temp_files, vec![path]);
    }

    #[test]
    fn with_temp_files_adds_multiple_paths() {
        let path1 = PathBuf::from("/test/path1.sock");
        let path2 = PathBuf::from("/test/path2.sock");
        let handle = create_test_handle().with_temp_files([path1.clone(), path2.clone()]);
        assert_eq!(handle.temp_files, vec![path1, path2]);
    }

    #[test]
    fn with_temp_file_accumulates() {
        let path1 = PathBuf::from("/test/path1.efivarstore");
        let path2 = PathBuf::from("/test/path2.sock");
        let handle = create_test_handle()
            .with_temp_file(path1.clone())
            .with_temp_file(path2.clone());
        assert_eq!(handle.temp_files, vec![path1, path2]);
    }

    fn create_test_handle() -> VmHandle {
        VmHandle {
            backend_handle: Arc::new(Box::new(MockBackendHandle)),
            status: AtomicU8::new(STATUS_RUNNING),
            exit_code: std::sync::Mutex::new(None),
            error_message: std::sync::Mutex::new(None),
            guest_os: GuestOs::Linux,
            resources: ResourceConfig::default(),
            temp_files: Vec::new(),
            vsock_sockets: HashMap::new(),
        }
    }

    #[test]
    fn vsock_socket_returns_socket_for_configured_port() {
        let mut config = VsockConfig::default();
        config.add_port(capsa_core::VsockPortConfig::listen(1024, "/tmp/test.sock"));
        let handle = create_test_handle().with_vsock_config(&config);

        let socket = handle.vsock_socket(1024).expect("socket should exist");
        assert_eq!(socket.port(), 1024);
        assert_eq!(socket.path(), std::path::Path::new("/tmp/test.sock"));
    }

    #[test]
    fn vsock_socket_returns_none_for_unconfigured_port() {
        let handle = create_test_handle();
        assert!(handle.vsock_socket(1024).is_none());
    }

    #[test]
    fn vsock_sockets_returns_all_mappings() {
        let mut config = VsockConfig::default();
        config.add_port(capsa_core::VsockPortConfig::listen(1024, "/tmp/a.sock"));
        config.add_port(capsa_core::VsockPortConfig::connect(2048, "/tmp/b.sock"));
        let handle = create_test_handle().with_vsock_config(&config);

        assert_eq!(handle.vsock_sockets().len(), 2);

        let socket_a = handle
            .vsock_sockets()
            .get(&1024)
            .expect("socket should exist");
        assert_eq!(socket_a.port(), 1024);
        assert_eq!(socket_a.path(), std::path::Path::new("/tmp/a.sock"));

        let socket_b = handle
            .vsock_sockets()
            .get(&2048)
            .expect("socket should exist");
        assert_eq!(socket_b.port(), 2048);
        assert_eq!(socket_b.path(), std::path::Path::new("/tmp/b.sock"));
    }

    struct MockBackendHandle;

    #[async_trait]
    impl BackendVmHandle for MockBackendHandle {
        async fn is_running(&self) -> bool {
            true
        }

        async fn shutdown(&self) -> Result<()> {
            Ok(())
        }

        async fn kill(&self) -> Result<()> {
            Ok(())
        }

        async fn wait(&self) -> Result<i32> {
            Ok(0)
        }

        async fn console_stream(&self) -> Result<Option<ConsoleStream>> {
            Ok(None)
        }
    }

    #[tokio::test]
    async fn kill_cleans_up_temp_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();
        temp_file.keep().unwrap();

        assert!(path.exists());

        let handle = create_test_handle().with_temp_file(path.clone());
        handle.kill().await.unwrap();

        assert!(!path.exists(), "Temp file should be deleted after kill");
    }

    #[tokio::test]
    async fn wait_cleans_up_temp_file() {
        let temp_file = NamedTempFile::new().unwrap();
        let path = temp_file.path().to_path_buf();
        temp_file.keep().unwrap();

        assert!(path.exists());

        let handle = create_test_handle().with_temp_file(path.clone());
        handle.wait().await.unwrap();

        assert!(!path.exists(), "Temp file should be deleted after wait");
    }
}
