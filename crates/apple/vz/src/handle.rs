//! VM handle for managing a running virtual machine.

use crate::delegate::{StopReceiver, VmStateDelegate, VmStopReason};
use async_trait::async_trait;
use block2::RcBlock;
use capsa_core::{AsyncPipe, BackendVmHandle, ConsoleStream, Error, Result};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use objc2::rc::Retained;
use objc2_foundation::NSError;
use objc2_virtualization::VZVirtualMachine;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Mutex;

pub struct NativeVmHandle {
    vm_addr: AtomicUsize,
    delegate_addr: AtomicUsize,
    running: AtomicBool,
    console_read_fd: Option<Mutex<Option<OwnedFd>>>,
    console_write_fd: Option<Mutex<Option<OwnedFd>>>,
    stop_receiver: Mutex<Option<StopReceiver>>,
}

impl NativeVmHandle {
    pub fn new(
        vm_addr: usize,
        delegate_addr: usize,
        console_read_fd: Option<OwnedFd>,
        console_write_fd: Option<OwnedFd>,
        stop_receiver: StopReceiver,
    ) -> Self {
        Self {
            vm_addr: AtomicUsize::new(vm_addr),
            delegate_addr: AtomicUsize::new(delegate_addr),
            running: AtomicBool::new(true),
            console_read_fd: console_read_fd.map(|fd| Mutex::new(Some(fd))),
            console_write_fd: console_write_fd.map(|fd| Mutex::new(Some(fd))),
            stop_receiver: Mutex::new(Some(stop_receiver)),
        }
    }

    fn get_vm_addr(&self) -> usize {
        self.vm_addr.load(Ordering::SeqCst)
    }
}

impl Drop for NativeVmHandle {
    fn drop(&mut self) {
        let vm_addr = self.vm_addr.load(Ordering::SeqCst);
        let delegate_addr = self.delegate_addr.load(Ordering::SeqCst);

        if vm_addr != 0 || delegate_addr != 0 {
            // SAFETY: We reclaim ownership of the VZVirtualMachine and VmStateDelegate to drop them.
            // The pointers were created by create_vm and have been kept alive by this handle.
            // Both must be accessed from the main thread, hence on_main_sync.
            // We clear the delegate before dropping the VM to prevent use-after-free if the
            // VM tries to call the delegate during teardown.
            apple_main::on_main_sync(move || unsafe {
                if vm_addr != 0 {
                    let vm_ptr = vm_addr as *mut VZVirtualMachine;
                    let vm = Retained::from_raw(vm_ptr).expect("Invalid VM pointer");
                    vm.setDelegate(None);
                }
                if delegate_addr != 0 {
                    let delegate_ptr = delegate_addr as *mut VmStateDelegate;
                    let _ = Retained::from_raw(delegate_ptr);
                }
            });
        }
    }
}

#[async_trait]
impl BackendVmHandle for NativeVmHandle {
    async fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    async fn wait(&self) -> Result<i32> {
        let receiver = {
            let mut guard = self.stop_receiver.lock().await;
            guard.take()
        };

        let Some(receiver) = receiver else {
            return if self.running.load(Ordering::SeqCst) {
                Err(Error::Hypervisor(
                    "wait() called multiple times".to_string(),
                ))
            } else {
                Ok(0)
            };
        };

        let stop_reason = tokio::task::spawn_blocking(move || receiver.recv()).await;

        self.running.store(false, Ordering::SeqCst);

        match stop_reason {
            Ok(Ok(VmStopReason::GuestStopped)) => Ok(0),
            Ok(Ok(VmStopReason::Error(msg))) => {
                Err(Error::Hypervisor(format!("VM stopped with error: {}", msg)))
            }
            Ok(Err(_)) => Err(Error::Hypervisor(
                "VM state channel disconnected unexpectedly".to_string(),
            )),
            Err(_) => Err(Error::Hypervisor("wait task panicked".to_string())),
        }
    }

    async fn shutdown(&self) -> Result<()> {
        let addr = self.get_vm_addr();
        // SAFETY: The VM pointer was created by create_vm and is kept alive by this handle.
        // VZVirtualMachine is accessed on the main thread as required by the framework.
        // We check canRequestStop() before calling requestStopWithError() as per the API.
        apple_main::on_main(move || unsafe {
            let ptr = addr as *const VZVirtualMachine;
            if (*ptr).canRequestStop() {
                let _ = (*ptr).requestStopWithError();
            }
        })
        .await;
        Ok(())
    }

    async fn kill(&self) -> Result<()> {
        let addr = self.get_vm_addr();
        let (result_tx, result_rx) = tokio::sync::oneshot::channel();

        // SAFETY: The VM pointer was created by create_vm and is kept alive by this handle.
        // VZVirtualMachine is accessed on the main thread as required by the framework.
        // The completion handler is leaked via mem::forget because the Objective-C runtime
        // retains it and will call it when the stop operation completes.
        apple_main::on_main(move || unsafe {
            let ptr = addr as *const VZVirtualMachine;

            let result_tx = std::sync::Mutex::new(Some(result_tx));
            let completion_handler = RcBlock::new(move |error: *mut NSError| {
                if let Some(tx) = result_tx.lock().unwrap().take() {
                    let _ = tx.send(error.is_null());
                }
            });

            std::mem::forget(completion_handler.clone());

            (*ptr).stopWithCompletionHandler(&completion_handler);
        })
        .await;

        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), result_rx).await;
        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    async fn console_stream(&self) -> Result<Option<ConsoleStream>> {
        let (Some(read_fd_mutex), Some(write_fd_mutex)) =
            (&self.console_read_fd, &self.console_write_fd)
        else {
            return Ok(None);
        };

        let mut read_guard = read_fd_mutex.lock().await;
        let mut write_guard = write_fd_mutex.lock().await;

        let Some(read_fd) = read_guard.take() else {
            return Err(Error::ConsoleNotEnabled);
        };
        let Some(write_fd) = write_guard.take() else {
            return Err(Error::ConsoleNotEnabled);
        };

        for fd in [&read_fd, &write_fd] {
            let flags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)
                .map_err(|e| Error::StartFailed(format!("fcntl failed: {}", e)))?;
            let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
            fcntl(fd.as_raw_fd(), FcntlArg::F_SETFL(flags))
                .map_err(|e| Error::StartFailed(format!("fcntl failed: {}", e)))?;
        }

        let async_pipe = AsyncPipe::new(read_fd, write_fd)
            .map_err(|e| Error::StartFailed(format!("AsyncPipe failed: {}", e)))?;

        Ok(Some(Box::new(async_pipe)))
    }
}
