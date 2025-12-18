//! Native Virtualization.framework backend for macOS.
//!
//! This backend uses Apple's Virtualization.framework directly instead of
//! spawning vfkit as a subprocess, eliminating process spawn overhead.
//!
//! SAFETY: VZVirtualMachine is not thread-safe and must only be accessed from
//! the main thread. This module uses apple_main::on_main to ensure all VM
//! operations happen on the main queue.

use super::{BackendVmHandle, ConsoleStream, HypervisorBackend, InternalVmConfig};
use crate::boot::KernelCmdline;
use crate::capabilities::{
    BackendCapabilities, BootMethodSupport, GuestOsSupport, ImageFormatSupport, NetworkModeSupport,
};
use crate::error::{Error, Result};
use crate::types::{ConsoleMode, NetworkMode};
use async_trait::async_trait;
use block2::RcBlock;
use nix::fcntl::{fcntl, FcntlArg, OFlag};
use objc2::rc::Retained;
use objc2::AllocAnyThread;
use objc2_foundation::{NSError, NSString, NSURL};
use objc2_virtualization::{
    VZLinuxBootLoader, VZNATNetworkDeviceAttachment, VZVirtioConsoleDeviceSerialPortConfiguration,
    VZVirtioNetworkDeviceConfiguration, VZVirtualMachine, VZVirtualMachineConfiguration,
    VZVirtualMachineState,
};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::io::unix::AsyncFd;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::sync::Mutex;

pub struct NativeVirtualizationBackend {
    capabilities: BackendCapabilities,
}

impl NativeVirtualizationBackend {
    pub fn new() -> Self {
        let capabilities = BackendCapabilities {
            guest_os: GuestOsSupport { linux: true },
            boot_methods: BootMethodSupport { linux_direct: true },
            image_formats: ImageFormatSupport {
                raw: true,
                qcow2: false,
            },
            network_modes: NetworkModeSupport {
                none: true,
                nat: true,
            },
            virtio_fs: true,
            virtio_9p: false,
            vsock: true,
            max_cpus: None,
            max_memory_mb: None,
        };

        Self { capabilities }
    }
}

impl Default for NativeVirtualizationBackend {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl HypervisorBackend for NativeVirtualizationBackend {
    fn name(&self) -> &'static str {
        "native-virtualization"
    }

    fn capabilities(&self) -> &BackendCapabilities {
        &self.capabilities
    }

    fn is_available(&self) -> bool {
        // The native backend requires the main thread to have an active runloop.
        // When using #[apple_main::main], the runloop is always active.
        // We verify by attempting a dispatch to main queue with timeout.
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        static CHECKED: AtomicBool = AtomicBool::new(false);
        static AVAILABLE: AtomicBool = AtomicBool::new(false);

        if CHECKED.load(Ordering::SeqCst) {
            return AVAILABLE.load(Ordering::SeqCst);
        }

        let (tx, rx) = std::sync::mpsc::channel();
        dispatch::Queue::main().exec_async(move || {
            let _ = tx.send(());
        });

        // If the runloop is active, the dispatch should complete quickly
        let available = rx.recv_timeout(Duration::from_millis(100)).is_ok();
        AVAILABLE.store(available, Ordering::SeqCst);
        CHECKED.store(true, Ordering::SeqCst);
        available
    }

    async fn start(&self, config: &InternalVmConfig) -> Result<Box<dyn BackendVmHandle>> {
        // Console needs two pipes:
        // - Input pipe: host writes -> VM reads (for keyboard input)
        // - Output pipe: VM writes -> host reads (for console output)
        let (console_input_read, console_input_write, console_output_read, console_output_write) =
            if config.console != ConsoleMode::Disabled {
                let (input_read, input_write) = create_pipe()?;
                let (output_read, output_write) = create_pipe()?;
                (
                    Some(input_read),
                    Some(input_write),
                    Some(output_read),
                    Some(output_write),
                )
            } else {
                (None, None, None, None)
            };

        let kernel_path = config.kernel.clone();
        let initrd_path = config.initrd.clone();
        let cmdline = config.cmdline.clone();
        let cpus = config.resources.cpus;
        let memory_mb = config.resources.memory_mb as u64;
        let network = config.network.clone();
        let console_input_read_fd = console_input_read.as_ref().map(|fd| fd.as_raw_fd());
        let console_output_write_fd = console_output_write.as_ref().map(|fd| fd.as_raw_fd());

        // Create VM on main queue
        let vm_addr = apple_main::on_main(move || {
            create_vm(
                &kernel_path,
                &initrd_path,
                &cmdline,
                cpus,
                memory_mb,
                network,
                console_input_read_fd,
                console_output_write_fd,
            )
        })
        .await
        .map_err(|e| Error::StartFailed(format!("VM creation failed: {}", e)))?;

        // Start VM on main queue with completion handler
        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        apple_main::on_main(move || {
            start_vm(vm_addr, start_tx);
        })
        .await;

        // Wait for VM start completion
        match tokio::time::timeout(std::time::Duration::from_secs(30), start_rx).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => return Err(Error::StartFailed(format!("VM start failed: {}", e))),
            Ok(Err(_)) => return Err(Error::StartFailed("VM start channel closed".to_string())),
            Err(_) => return Err(Error::StartFailed("VM start timed out".to_string())),
        }

        // Drop the FDs that the VM owns (VM reads from input_read, writes to output_write)
        // Keep the FDs that the host uses (host writes to input_write, reads from output_read)
        drop(console_input_read);
        drop(console_output_write);

        Ok(Box::new(NativeVmHandle::new(
            vm_addr,
            console_output_read,
            console_input_write,
        )))
    }

    fn kernel_cmdline_defaults(&self) -> KernelCmdline {
        let mut cmdline = KernelCmdline::new();
        cmdline.console("hvc0");
        cmdline.arg("reboot", "t");
        cmdline.arg("panic", "-1");
        cmdline
    }

    fn default_root_device(&self) -> &str {
        "/dev/vda"
    }
}

fn create_pipe() -> Result<(OwnedFd, OwnedFd)> {
    let mut fds = [0i32; 2];
    if unsafe { libc::pipe(fds.as_mut_ptr()) } != 0 {
        return Err(Error::StartFailed("Failed to create pipe".to_string()));
    }
    Ok(unsafe {
        (
            OwnedFd::from_raw_fd(fds[0]),
            OwnedFd::from_raw_fd(fds[1]),
        )
    })
}

fn create_vm(
    kernel_path: &std::path::Path,
    initrd_path: &std::path::Path,
    cmdline: &str,
    cpus: u32,
    memory_mb: u64,
    network: NetworkMode,
    console_input_read_fd: Option<RawFd>,
    console_output_write_fd: Option<RawFd>,
) -> Result<usize> {
    unsafe {
        let kernel_url = NSURL::fileURLWithPath(&NSString::from_str(
            kernel_path
                .to_str()
                .ok_or_else(|| Error::StartFailed("Invalid kernel path".to_string()))?,
        ));
        let boot_loader =
            VZLinuxBootLoader::initWithKernelURL(VZLinuxBootLoader::alloc(), &kernel_url);

        let initrd_url = NSURL::fileURLWithPath(&NSString::from_str(
            initrd_path
                .to_str()
                .ok_or_else(|| Error::StartFailed("Invalid initrd path".to_string()))?,
        ));
        boot_loader.setInitialRamdiskURL(Some(&initrd_url));
        boot_loader.setCommandLine(&NSString::from_str(cmdline));

        let vm_config = VZVirtualMachineConfiguration::new();
        vm_config.setBootLoader(Some(&boot_loader));
        vm_config.setCPUCount(cpus as usize);
        vm_config.setMemorySize(memory_mb * 1024 * 1024);

        if let NetworkMode::Nat = network {
            let net_attachment = VZNATNetworkDeviceAttachment::new();
            let net_config = VZVirtioNetworkDeviceConfiguration::new();
            net_config.setAttachment(Some(&net_attachment));

            let net_configs: Retained<
                objc2_foundation::NSArray<objc2_virtualization::VZNetworkDeviceConfiguration>,
            > = objc2_foundation::NSArray::from_retained_slice(&[
                objc2::rc::Retained::into_super(net_config),
            ]);
            vm_config.setNetworkDevices(&net_configs);
        }

        if let (Some(read_fd), Some(write_fd)) = (console_input_read_fd, console_output_write_fd) {
            let read_handle = objc2_foundation::NSFileHandle::initWithFileDescriptor(
                objc2_foundation::NSFileHandle::alloc(),
                read_fd,
            );
            let write_handle = objc2_foundation::NSFileHandle::initWithFileDescriptor(
                objc2_foundation::NSFileHandle::alloc(),
                write_fd,
            );

            let serial_attachment = objc2_virtualization::VZFileHandleSerialPortAttachment::initWithFileHandleForReading_fileHandleForWriting(
                objc2_virtualization::VZFileHandleSerialPortAttachment::alloc(),
                Some(&read_handle),
                Some(&write_handle),
            );

            let serial_config = VZVirtioConsoleDeviceSerialPortConfiguration::new();
            serial_config.setAttachment(Some(&serial_attachment));

            let serial_configs: Retained<
                objc2_foundation::NSArray<objc2_virtualization::VZSerialPortConfiguration>,
            > = objc2_foundation::NSArray::from_retained_slice(&[
                objc2::rc::Retained::into_super(serial_config),
            ]);
            vm_config.setSerialPorts(&serial_configs);
        }

        vm_config
            .validateWithError()
            .map_err(|e| Error::StartFailed(format!("VM config validation failed: {}", e)))?;

        let vm = VZVirtualMachine::initWithConfiguration(VZVirtualMachine::alloc(), &vm_config);
        let ptr = Retained::into_raw(vm);
        Ok(ptr as usize)
    }
}

fn start_vm(vm_addr: usize, result_tx: tokio::sync::oneshot::Sender<std::result::Result<(), String>>) {
    unsafe {
        let ptr = vm_addr as *mut VZVirtualMachine;
        let vm = Retained::from_raw(ptr).expect("Invalid VM pointer");

        // Wrap in Mutex<Option<>> to satisfy Fn trait (completion handler is called once)
        let result_tx = std::sync::Mutex::new(Some(result_tx));
        let completion_handler = RcBlock::new(move |error: *mut NSError| {
            if let Some(tx) = result_tx.lock().unwrap().take() {
                if error.is_null() {
                    let _ = tx.send(Ok(()));
                } else {
                    let err = &*error;
                    let _ = tx.send(Err(err.localizedDescription().to_string()));
                }
            }
        });

        // Keep the completion handler alive - it will be called asynchronously
        std::mem::forget(completion_handler.clone());

        vm.startWithCompletionHandler(&completion_handler);

        // Re-leak the VM so it stays alive
        let _ = Retained::into_raw(vm);
    }
}

struct NativeVmHandle {
    vm_addr: AtomicUsize,
    running: AtomicBool,
    console_read_fd: Option<Mutex<Option<OwnedFd>>>,
    console_write_fd: Option<Mutex<Option<OwnedFd>>>,
}

impl NativeVmHandle {
    fn new(vm_addr: usize, console_read_fd: Option<OwnedFd>, console_write_fd: Option<OwnedFd>) -> Self {
        Self {
            vm_addr: AtomicUsize::new(vm_addr),
            running: AtomicBool::new(true),
            console_read_fd: console_read_fd.map(|fd| Mutex::new(Some(fd))),
            console_write_fd: console_write_fd.map(|fd| Mutex::new(Some(fd))),
        }
    }

    fn get_vm_addr(&self) -> usize {
        self.vm_addr.load(Ordering::SeqCst)
    }
}

impl Drop for NativeVmHandle {
    fn drop(&mut self) {
        let addr = self.vm_addr.load(Ordering::SeqCst);
        if addr != 0 {
            // Clean up the VM on the main queue
            apple_main::on_main_sync(move || unsafe {
                let ptr = addr as *mut VZVirtualMachine;
                let _ = Retained::from_raw(ptr);
            });
        }
    }
}

#[async_trait]
impl BackendVmHandle for NativeVmHandle {
    fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    async fn wait(&self) -> Result<i32> {
        loop {
            let addr = self.get_vm_addr();
            let state = apple_main::on_main(move || unsafe {
                let ptr = addr as *const VZVirtualMachine;
                (*ptr).state()
            })
            .await;

            match state {
                VZVirtualMachineState::Stopped | VZVirtualMachineState::Error => {
                    self.running.store(false, Ordering::SeqCst);
                    return Ok(0);
                }
                _ => {}
            }
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        }
    }

    async fn shutdown(&self) -> Result<()> {
        let addr = self.get_vm_addr();
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

        apple_main::on_main(move || unsafe {
            let ptr = addr as *const VZVirtualMachine;

            // Wrap in Mutex<Option<>> to satisfy Fn trait (completion handler is called once)
            let result_tx = std::sync::Mutex::new(Some(result_tx));
            let completion_handler = RcBlock::new(move |error: *mut NSError| {
                if let Some(tx) = result_tx.lock().unwrap().take() {
                    let _ = tx.send(error.is_null());
                }
            });

            // Keep the completion handler alive
            std::mem::forget(completion_handler.clone());

            (*ptr).stopWithCompletionHandler(&completion_handler);
        })
        .await;

        // Wait for completion
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

        // Set both FDs to non-blocking
        for fd in [&read_fd, &write_fd] {
            let flags = fcntl(fd.as_raw_fd(), FcntlArg::F_GETFL)
                .map_err(|e| Error::StartFailed(format!("fcntl failed: {}", e)))?;
            let flags = OFlag::from_bits_truncate(flags) | OFlag::O_NONBLOCK;
            fcntl(fd.as_raw_fd(), FcntlArg::F_SETFL(flags))
                .map_err(|e| Error::StartFailed(format!("fcntl failed: {}", e)))?;
        }

        let async_read_fd =
            AsyncFd::new(read_fd).map_err(|e| Error::StartFailed(format!("AsyncFd failed: {}", e)))?;
        let async_write_fd =
            AsyncFd::new(write_fd).map_err(|e| Error::StartFailed(format!("AsyncFd failed: {}", e)))?;

        Ok(Some(Box::new(AsyncConsolePipe {
            read_fd: async_read_fd,
            write_fd: async_write_fd,
        })))
    }
}

struct AsyncConsolePipe {
    read_fd: AsyncFd<OwnedFd>,
    write_fd: AsyncFd<OwnedFd>,
}

impl AsyncRead for AsyncConsolePipe {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        loop {
            let mut guard = match self.read_fd.poll_read_ready(cx) {
                std::task::Poll::Ready(Ok(guard)) => guard,
                std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };

            let fd = self.read_fd.get_ref().as_raw_fd();
            let unfilled = buf.initialize_unfilled();

            match nix::unistd::read(fd, unfilled) {
                Ok(n) => {
                    buf.advance(n);
                    return std::task::Poll::Ready(Ok(()));
                }
                Err(nix::errno::Errno::EAGAIN) => {
                    guard.clear_ready();
                    continue;
                }
                Err(e) => {
                    return std::task::Poll::Ready(Err(std::io::Error::other(e)));
                }
            }
        }
    }
}

impl AsyncWrite for AsyncConsolePipe {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut std::task::Context<'_>,
        buf: &[u8],
    ) -> std::task::Poll<std::io::Result<usize>> {
        loop {
            let mut guard = match self.write_fd.poll_write_ready(cx) {
                std::task::Poll::Ready(Ok(guard)) => guard,
                std::task::Poll::Ready(Err(e)) => return std::task::Poll::Ready(Err(e)),
                std::task::Poll::Pending => return std::task::Poll::Pending,
            };

            match nix::unistd::write(self.write_fd.get_ref(), buf) {
                Ok(n) => return std::task::Poll::Ready(Ok(n)),
                Err(nix::errno::Errno::EAGAIN) => {
                    guard.clear_ready();
                    continue;
                }
                Err(e) => {
                    return std::task::Poll::Ready(Err(std::io::Error::other(e)));
                }
            }
        }
    }

    fn poll_flush(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        _cx: &mut std::task::Context<'_>,
    ) -> std::task::Poll<std::io::Result<()>> {
        std::task::Poll::Ready(Ok(()))
    }
}
