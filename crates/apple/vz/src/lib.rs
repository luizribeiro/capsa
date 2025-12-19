//! Native Virtualization.framework backend for macOS.
//!
//! This backend uses Apple's Virtualization.framework directly instead of
//! spawning vfkit as a subprocess, eliminating process spawn overhead.
//!
//! SAFETY: VZVirtualMachine is not thread-safe and must only be accessed from
//! the main thread. This module uses apple_main::on_main to ensure all VM
//! operations happen on the main queue.

// TODO: make sure all capabilities are covered by tests using the minimal VM
// TODO: split this file further (mod.rs, handle.rs, console.rs?, etc)

use async_trait::async_trait;
use block2::RcBlock;
use capsa_core::{
    AsyncPipe, BackendCapabilities, BackendVmHandle, ConsoleMode, ConsoleStream,
    DEFAULT_ROOT_DEVICE, Error, HypervisorBackend, KernelCmdline, NetworkMode, Result, VmConfig,
    macos_cmdline_defaults, macos_virtualization_capabilities,
};
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, DefinedClass, MainThreadMarker, MainThreadOnly, define_class};
use objc2_foundation::{NSError, NSObject, NSObjectProtocol, NSString, NSURL};
use objc2_virtualization::{
    VZLinuxBootLoader, VZNATNetworkDeviceAttachment, VZVirtioConsoleDeviceSerialPortConfiguration,
    VZVirtioNetworkDeviceConfiguration, VZVirtualMachine, VZVirtualMachineConfiguration,
    VZVirtualMachineDelegate,
};
use std::cell::Cell;
use std::os::fd::{AsRawFd, OwnedFd};
use std::os::unix::io::RawFd;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::sync::Mutex;

type StopSender = std::sync::mpsc::SyncSender<VmStopReason>;

#[derive(Debug, Clone)]
enum VmStopReason {
    GuestStopped,
    Error(String),
}

struct VmStateDelegateIvars {
    stop_sender: Cell<Option<StopSender>>,
}

define_class!(
    // SAFETY:
    // - NSObject has no subclassing requirements
    // - We don't implement Drop
    #[unsafe(super(NSObject))]
    #[thread_kind = MainThreadOnly]
    #[ivars = VmStateDelegateIvars]
    struct VmStateDelegate;

    unsafe impl NSObjectProtocol for VmStateDelegate {}

    unsafe impl VZVirtualMachineDelegate for VmStateDelegate {
        #[unsafe(method(guestDidStopVirtualMachine:))]
        fn guest_did_stop(&self, _vm: &VZVirtualMachine) {
            let sender: Option<StopSender> = self.ivars().stop_sender.take();
            if let Some(sender) = sender {
                let _ = sender.try_send(VmStopReason::GuestStopped);
            }
        }

        #[unsafe(method(virtualMachine:didStopWithError:))]
        fn vm_did_stop_with_error(&self, _vm: &VZVirtualMachine, error: &NSError) {
            let sender: Option<StopSender> = self.ivars().stop_sender.take();
            if let Some(sender) = sender {
                let error_msg = error.localizedDescription().to_string();
                let _ = sender.try_send(VmStopReason::Error(error_msg));
            }
        }
    }
);

impl VmStateDelegate {
    fn new(mtm: MainThreadMarker, stop_sender: StopSender) -> Retained<Self> {
        let this = Self::alloc(mtm);
        let this = this.set_ivars(VmStateDelegateIvars {
            stop_sender: Cell::new(Some(stop_sender)),
        });
        // SAFETY: Calling init on a freshly allocated NSObject subclass
        unsafe { objc2::msg_send![super(this), init] }
    }
}

pub struct NativeVirtualizationBackend {
    capabilities: BackendCapabilities,
}

impl NativeVirtualizationBackend {
    pub fn new() -> Self {
        Self {
            capabilities: macos_virtualization_capabilities(),
        }
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

        let available = rx.recv_timeout(Duration::from_millis(100)).is_ok();
        AVAILABLE.store(available, Ordering::SeqCst);
        CHECKED.store(true, Ordering::SeqCst);
        available
    }

    async fn start(&self, config: &VmConfig) -> Result<Box<dyn BackendVmHandle>> {
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

        let (stop_tx, stop_rx) = std::sync::mpsc::sync_channel(1);

        let vm_config = CreateVmConfig {
            kernel_path: config.kernel.clone(),
            initrd_path: config.initrd.clone(),
            cmdline: config.cmdline.clone(),
            cpus: config.resources.cpus,
            memory_mb: config.resources.memory_mb as u64,
            network: config.network.clone(),
            console_input_read_fd: console_input_read.as_ref().map(|fd| fd.as_raw_fd()),
            console_output_write_fd: console_output_write.as_ref().map(|fd| fd.as_raw_fd()),
            stop_sender: stop_tx,
        };

        let (vm_addr, delegate_addr) = apple_main::on_main(move || create_vm(vm_config))
            .await
            .map_err(|e| Error::StartFailed(format!("VM creation failed: {}", e)))?;

        let (start_tx, start_rx) = tokio::sync::oneshot::channel();
        apple_main::on_main(move || {
            start_vm(vm_addr, start_tx);
        })
        .await;

        match tokio::time::timeout(std::time::Duration::from_secs(30), start_rx).await {
            Ok(Ok(Ok(()))) => {}
            Ok(Ok(Err(e))) => return Err(Error::StartFailed(format!("VM start failed: {}", e))),
            Ok(Err(_)) => return Err(Error::StartFailed("VM start channel closed".to_string())),
            Err(_) => return Err(Error::StartFailed("VM start timed out".to_string())),
        }

        drop(console_input_read);
        drop(console_output_write);

        Ok(Box::new(NativeVmHandle::new(
            vm_addr,
            delegate_addr,
            console_output_read,
            console_input_write,
            stop_rx,
        )))
    }

    fn kernel_cmdline_defaults(&self) -> KernelCmdline {
        macos_cmdline_defaults()
    }

    fn default_root_device(&self) -> &str {
        DEFAULT_ROOT_DEVICE
    }
}

fn create_pipe() -> Result<(OwnedFd, OwnedFd)> {
    nix::unistd::pipe().map_err(|e| Error::StartFailed(format!("Failed to create pipe: {}", e)))
}

struct CreateVmConfig {
    kernel_path: std::path::PathBuf,
    initrd_path: std::path::PathBuf,
    cmdline: String,
    cpus: u32,
    memory_mb: u64,
    network: NetworkMode,
    console_input_read_fd: Option<RawFd>,
    console_output_write_fd: Option<RawFd>,
    stop_sender: StopSender,
}

fn create_vm(config: CreateVmConfig) -> Result<(usize, usize)> {
    let kernel_path_str = config
        .kernel_path
        .to_str()
        .ok_or_else(|| Error::StartFailed("Invalid kernel path".to_string()))?;
    let initrd_path_str = config
        .initrd_path
        .to_str()
        .ok_or_else(|| Error::StartFailed("Invalid initrd path".to_string()))?;

    // SAFETY: This block uses Objective-C FFI via objc2 bindings to Apple's
    // Virtualization.framework. The safety requirements are:
    // 1. All objc2 types (NSURL, NSString, VZ*) are used according to their API contracts
    // 2. The VZVirtualMachine is converted to a raw pointer via Retained::into_raw to
    //    transfer ownership out of this function. The caller (via NativeVmHandle) is
    //    responsible for eventually reclaiming it with Retained::from_raw.
    // 3. File descriptors passed for console I/O must be valid open descriptors.
    //    NSFileHandle takes ownership and will manage their lifetime.
    unsafe {
        let kernel_url = NSURL::fileURLWithPath(&NSString::from_str(kernel_path_str));
        let boot_loader =
            VZLinuxBootLoader::initWithKernelURL(VZLinuxBootLoader::alloc(), &kernel_url);

        let initrd_url = NSURL::fileURLWithPath(&NSString::from_str(initrd_path_str));
        boot_loader.setInitialRamdiskURL(Some(&initrd_url));
        boot_loader.setCommandLine(&NSString::from_str(&config.cmdline));

        let vm_config = VZVirtualMachineConfiguration::new();
        vm_config.setBootLoader(Some(&boot_loader));
        vm_config.setCPUCount(config.cpus as usize);
        vm_config.setMemorySize(config.memory_mb * 1024 * 1024);

        // TODO: setup root disk if config.disk is Some

        if let NetworkMode::Nat = config.network {
            let net_attachment = VZNATNetworkDeviceAttachment::new();
            let net_config = VZVirtioNetworkDeviceConfiguration::new();
            net_config.setAttachment(Some(&net_attachment));

            let net_configs: Retained<
                objc2_foundation::NSArray<objc2_virtualization::VZNetworkDeviceConfiguration>,
            > = objc2_foundation::NSArray::from_retained_slice(&[objc2::rc::Retained::into_super(
                net_config,
            )]);
            vm_config.setNetworkDevices(&net_configs);
        }

        if let (Some(read_fd), Some(write_fd)) =
            (config.console_input_read_fd, config.console_output_write_fd)
        {
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
            > = objc2_foundation::NSArray::from_retained_slice(&[objc2::rc::Retained::into_super(
                serial_config,
            )]);
            vm_config.setSerialPorts(&serial_configs);
        }

        vm_config
            .validateWithError()
            .map_err(|e| Error::StartFailed(format!("VM config validation failed: {}", e)))?;

        let vm = VZVirtualMachine::initWithConfiguration(VZVirtualMachine::alloc(), &vm_config);

        let mtm = MainThreadMarker::new().expect("create_vm must run on main thread");
        let delegate = VmStateDelegate::new(mtm, config.stop_sender);
        vm.setDelegate(Some(ProtocolObject::from_ref(&*delegate)));

        let vm_ptr = Retained::into_raw(vm);
        let delegate_ptr = Retained::into_raw(delegate);
        Ok((vm_ptr as usize, delegate_ptr as usize))
    }
}

fn start_vm(
    vm_addr: usize,
    result_tx: tokio::sync::oneshot::Sender<std::result::Result<(), String>>,
) {
    // SAFETY: This function reclaims ownership of a VZVirtualMachine from a raw pointer.
    // Requirements:
    // 1. vm_addr must have been created by create_vm via Retained::into_raw
    // 2. This function temporarily takes ownership to call startWithCompletionHandler,
    //    then releases it back via into_raw so the VM continues to exist
    // 3. The completion handler is leaked via mem::forget because the Objective-C runtime
    //    retains it and we cannot know when it will be released
    // 4. The error pointer in the completion handler is only dereferenced if non-null,
    //    as guaranteed by the Virtualization.framework API contract
    unsafe {
        let ptr = vm_addr as *mut VZVirtualMachine;
        let vm = Retained::from_raw(ptr).expect("Invalid VM pointer");

        let result_tx = std::sync::Mutex::new(Some(result_tx));
        let completion_handler = RcBlock::new(move |error: *mut NSError| {
            if let Some(tx) = result_tx.lock().unwrap().take() {
                if error.is_null() {
                    let _ = tx.send(Ok(()));
                } else {
                    // SAFETY: error is non-null, and the Virtualization.framework guarantees
                    // it points to a valid NSError when the start operation fails
                    let err = &*error;
                    let _ = tx.send(Err(err.localizedDescription().to_string()));
                }
            }
        });

        std::mem::forget(completion_handler.clone());

        vm.startWithCompletionHandler(&completion_handler);

        let _ = Retained::into_raw(vm);
    }
}

type StopReceiver = std::sync::mpsc::Receiver<VmStopReason>;

struct NativeVmHandle {
    vm_addr: AtomicUsize,
    delegate_addr: AtomicUsize,
    running: AtomicBool,
    console_read_fd: Option<Mutex<Option<OwnedFd>>>,
    console_write_fd: Option<Mutex<Option<OwnedFd>>>,
    stop_receiver: Mutex<Option<StopReceiver>>,
}

impl NativeVmHandle {
    fn new(
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

#[cfg(test)]
mod tests {
    use super::*;

    mod backend_construction {
        use super::*;

        #[test]
        fn new_creates_backend_with_correct_capabilities() {
            let backend = NativeVirtualizationBackend::new();
            let caps = backend.capabilities();

            assert!(caps.guest_os.linux);
            assert!(caps.boot_methods.linux_direct);
            assert!(caps.image_formats.raw);
            assert!(!caps.image_formats.qcow2);
            assert!(caps.network_modes.none);
            assert!(caps.network_modes.nat);
            assert!(caps.share_mechanisms.virtio_fs);
            assert!(!caps.share_mechanisms.virtio_9p);
            assert!(caps.max_cpus.is_none());
            assert!(caps.max_memory_mb.is_none());
        }

        #[test]
        fn default_creates_same_as_new() {
            let backend1 = NativeVirtualizationBackend::new();
            let backend2 = NativeVirtualizationBackend::default();

            let caps1 = backend1.capabilities();
            let caps2 = backend2.capabilities();

            assert_eq!(caps1.guest_os.linux, caps2.guest_os.linux);
            assert_eq!(
                caps1.boot_methods.linux_direct,
                caps2.boot_methods.linux_direct
            );
            assert_eq!(caps1.image_formats.raw, caps2.image_formats.raw);
        }

        #[test]
        fn name_returns_correct_name() {
            let backend = NativeVirtualizationBackend::new();
            assert_eq!(backend.name(), "native-virtualization");
        }
    }

    mod kernel_cmdline {
        use super::*;

        #[test]
        fn defaults_sets_console_to_hvc0() {
            let backend = NativeVirtualizationBackend::new();
            let cmdline = backend.kernel_cmdline_defaults();
            assert_eq!(cmdline.get("console"), Some("hvc0"));
        }

        #[test]
        fn defaults_sets_reboot_to_t() {
            let backend = NativeVirtualizationBackend::new();
            let cmdline = backend.kernel_cmdline_defaults();
            assert_eq!(cmdline.get("reboot"), Some("t"));
        }

        #[test]
        fn defaults_sets_panic_to_minus_one() {
            let backend = NativeVirtualizationBackend::new();
            let cmdline = backend.kernel_cmdline_defaults();
            assert_eq!(cmdline.get("panic"), Some("-1"));
        }

        #[test]
        fn default_root_device_is_vda() {
            let backend = NativeVirtualizationBackend::new();
            assert_eq!(backend.default_root_device(), "/dev/vda");
        }
    }

    mod pipe_creation {
        use super::*;

        #[test]
        fn create_pipe_returns_two_fds() {
            let result = create_pipe();
            assert!(result.is_ok());

            let (read_fd, write_fd) = result.unwrap();
            assert!(read_fd.as_raw_fd() >= 0);
            assert!(write_fd.as_raw_fd() >= 0);
            assert_ne!(read_fd.as_raw_fd(), write_fd.as_raw_fd());
        }

        #[test]
        fn pipe_is_readable_and_writable() {
            let (read_fd, write_fd) = create_pipe().unwrap();

            let test_data = b"hello";
            let written = nix::unistd::write(&write_fd, test_data).unwrap();
            assert_eq!(written, test_data.len());

            let mut buf = [0u8; 5];
            let read = nix::unistd::read(read_fd.as_raw_fd(), &mut buf).unwrap();
            assert_eq!(read, test_data.len());
            assert_eq!(&buf, test_data);
        }
    }

    mod vm_stop_channel {
        use super::*;

        #[test]
        fn channel_receives_guest_stopped() {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            tx.try_send(VmStopReason::GuestStopped).unwrap();

            let result = rx.recv().unwrap();
            assert!(matches!(result, VmStopReason::GuestStopped));
        }

        #[test]
        fn channel_receives_error_with_message() {
            let (tx, rx) = std::sync::mpsc::sync_channel(1);
            tx.try_send(VmStopReason::Error("test error".to_string()))
                .unwrap();

            let result = rx.recv().unwrap();
            match result {
                VmStopReason::Error(msg) => assert_eq!(msg, "test error"),
                _ => panic!("Expected Error variant"),
            }
        }

        #[test]
        fn channel_disconnection_detected() {
            let (tx, rx) = std::sync::mpsc::sync_channel::<VmStopReason>(1);
            drop(tx);

            let result = rx.recv();
            assert!(result.is_err());
        }

        #[test]
        fn try_send_on_full_channel_fails() {
            let (tx, _rx) = std::sync::mpsc::sync_channel(1);
            tx.try_send(VmStopReason::GuestStopped).unwrap();

            let result = tx.try_send(VmStopReason::GuestStopped);
            assert!(result.is_err());
        }
    }

    mod vm_stop_reason {
        use super::*;

        #[test]
        fn guest_stopped_is_cloneable() {
            let reason = VmStopReason::GuestStopped;
            let cloned = reason.clone();
            assert!(matches!(cloned, VmStopReason::GuestStopped));
        }

        #[test]
        fn error_preserves_message() {
            let reason = VmStopReason::Error("something went wrong".to_string());
            let cloned = reason.clone();
            match cloned {
                VmStopReason::Error(msg) => assert_eq!(msg, "something went wrong"),
                _ => panic!("Expected Error variant"),
            }
        }

        #[test]
        fn debug_format_works() {
            let guest = VmStopReason::GuestStopped;
            let error = VmStopReason::Error("test".to_string());

            assert_eq!(format!("{:?}", guest), "GuestStopped");
            assert_eq!(format!("{:?}", error), "Error(\"test\")");
        }
    }
}
