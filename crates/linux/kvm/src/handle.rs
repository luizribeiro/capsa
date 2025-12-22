use async_trait::async_trait;
use capsa_core::{AsyncPipe, BackendVmHandle, ConsoleStream, Error, Result};
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use nix::sys::pthread::{pthread_kill, Pthread};
use nix::sys::signal::Signal;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread::JoinHandle;
use tokio::sync::{mpsc, Mutex};
use vm_memory::GuestMemoryMmap;

pub struct KvmVmHandle {
    running: Arc<AtomicBool>,
    exit_rx: Mutex<Option<mpsc::Receiver<i32>>>,
    vcpu_handles: Mutex<Vec<JoinHandle<()>>>,
    vcpu_thread_ids: Mutex<Vec<Pthread>>,
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
        vcpu_handles: Vec<JoinHandle<()>>,
        vcpu_thread_ids: Vec<Pthread>,
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
            let _ = pthread_kill(tid, Signal::SIGUSR1);
        }
        drop(thread_ids);

        let mut handles = self.vcpu_handles.lock().await;
        for handle in handles.drain(..) {
            let _ = handle.join();
        }

        Ok(())
    }

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
