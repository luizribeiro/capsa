use async_trait::async_trait;
use capsa_core::{AsyncPipe, BackendVmHandle, ConsoleStream, Error, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::pthread::{Pthread, pthread_kill};
use nix::sys::signal::Signal;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::{Mutex, mpsc};
use tokio::task::JoinHandle as TokioJoinHandle;
use vm_memory::GuestMemoryMmap;

pub struct KvmVmHandle {
    running: Arc<AtomicBool>,
    exit_rx: Mutex<Option<mpsc::Receiver<i32>>>,
    vcpu_handles: Mutex<Vec<std::thread::JoinHandle<()>>>,
    vcpu_thread_ids: Mutex<Vec<Pthread>>,
    console_input_task: Mutex<Option<TokioJoinHandle<()>>>,
    console_read_fd: Mutex<Option<OwnedFd>>,
    console_write_fd: Mutex<Option<OwnedFd>>,
    console_enabled: bool,
    #[allow(dead_code)]
    memory: GuestMemoryMmap, // Keep memory alive for the VM's lifetime
}

impl KvmVmHandle {
    pub fn new(
        running: Arc<AtomicBool>,
        exit_rx: mpsc::Receiver<i32>,
        vcpu_handles: Vec<std::thread::JoinHandle<()>>,
        vcpu_thread_ids: Vec<Pthread>,
        console_input_task: Option<TokioJoinHandle<()>>,
        console_read_fd: Option<OwnedFd>,
        console_write_fd: Option<OwnedFd>,
        console_enabled: bool,
        memory: GuestMemoryMmap,
    ) -> Self {
        Self {
            running,
            exit_rx: Mutex::new(Some(exit_rx)),
            vcpu_handles: Mutex::new(vcpu_handles),
            vcpu_thread_ids: Mutex::new(vcpu_thread_ids),
            console_input_task: Mutex::new(console_input_task),
            console_read_fd: Mutex::new(console_read_fd),
            console_write_fd: Mutex::new(console_write_fd),
            console_enabled,
            memory,
        }
    }
}

#[async_trait]
impl BackendVmHandle for KvmVmHandle {
    async fn is_running(&self) -> bool {
        self.running.load(Ordering::Relaxed)
    }

    async fn wait(&self) -> Result<i32> {
        let mut rx_guard = self.exit_rx.lock().await;
        if let Some(mut rx) = rx_guard.take() {
            drop(rx_guard);
            match rx.recv().await {
                Some(code) => Ok(code),
                None => Ok(0),
            }
        } else {
            Ok(0)
        }
    }

    async fn shutdown(&self) -> Result<()> {
        // TODO: Implement ACPI shutdown
        self.kill().await
    }

    async fn kill(&self) -> Result<()> {
        self.running.store(false, Ordering::Relaxed);

        // Send SIGUSR1 to each vCPU thread to interrupt vcpu.run()
        let thread_ids = self.vcpu_thread_ids.lock().await;
        for &tid in thread_ids.iter() {
            if let Err(e) = pthread_kill(tid, Signal::SIGUSR1) {
                tracing::warn!("failed to send SIGUSR1 to vCPU thread: {}", e);
            }
        }
        drop(thread_ids);

        let mut handles = self.vcpu_handles.lock().await;
        for handle in handles.drain(..) {
            let _ = handle.join();
        }

        // Close the console write pipe to signal EOF to the console input task
        tracing::debug!("kill: closing console write pipe");
        drop(self.console_write_fd.lock().await.take());

        // Abort and await the console input task if present
        tracing::debug!("kill: aborting console input task");
        if let Some(handle) = self.console_input_task.lock().await.take() {
            handle.abort();
            match handle.await {
                Err(e) if e.is_panic() => {
                    tracing::warn!("console input task panicked during shutdown");
                }
                _ => {}
            }
        }
        tracing::debug!("kill: done");

        Ok(())
    }

    /// Returns the console stream for this VM.
    ///
    /// This method can only be called once per VM instance. The console stream
    /// takes ownership of the underlying file descriptors, so subsequent calls
    /// will return `Error::ConsoleNotEnabled`.
    ///
    /// The returned stream provides bidirectional communication with the VM's
    /// serial console (ttyS0).
    async fn console_stream(&self) -> Result<Option<ConsoleStream>> {
        if !self.console_enabled {
            return Err(Error::ConsoleNotEnabled);
        }

        let read_fd = self.console_read_fd.lock().await.take();
        let write_fd = self.console_write_fd.lock().await.take();

        match (read_fd, write_fd) {
            (Some(read), Some(write)) => {
                set_nonblocking(&read)?;
                set_nonblocking(&write)?;
                let pipe = AsyncPipe::new(read, write)?;
                Ok(Some(Box::new(pipe)))
            }
            // Already claimed by a previous call
            _ => Err(Error::ConsoleNotEnabled),
        }
    }
}

fn set_nonblocking(fd: &OwnedFd) -> Result<()> {
    let flags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)
        .map_err(|e| Error::Io(std::io::Error::from_raw_os_error(e as i32)))?;
    let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
    fcntl(fd.as_raw_fd(), FcntlArg::F_SETFL(flags))
        .map_err(|e| Error::Io(std::io::Error::from_raw_os_error(e as i32)))?;
    Ok(())
}
