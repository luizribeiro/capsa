use crate::arch::{
    BOOT_PARAMS_ADDR, KERNEL_LOAD_ADDR, create_guest_memory, initrd_load_addr, run_vcpu,
    setup_boot_params, setup_regs, setup_sregs,
};
use crate::handle::KvmVmHandle;
use crate::serial::{SerialDevice, create_console_pipes};
use capsa_core::{BackendVmHandle, BootMethod, Error, Result, VmConfig};
use kvm_bindings::kvm_pit_config;
use kvm_ioctls::{Kvm, VmFd};
use linux_loader::loader::KernelLoader;
use linux_loader::loader::bzimage::BzImage;
use nix::fcntl::{FcntlArg, OFlag, fcntl};
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use tokio::sync::mpsc;
use vm_memory::{Address, Bytes, GuestAddress, GuestMemory, GuestMemoryMmap, GuestMemoryRegion};

static SIGNAL_HANDLER_INSTALLED: AtomicBool = AtomicBool::new(false);

/// Installs a no-op SIGUSR1 handler to interrupt blocking vcpu.run() calls.
///
/// When we need to stop a VM, we send SIGUSR1 to each vCPU thread. This causes
/// vcpu.run() to return with EINTR, allowing the thread to check the `running`
/// flag and exit gracefully.
///
/// This function is idempotent - calling it multiple times is safe and only
/// the first call actually installs the handler.
fn install_signal_handler() -> Result<()> {
    if SIGNAL_HANDLER_INSTALLED.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    extern "C" fn interrupt_handler(_: nix::libc::c_int) {}

    let handler = SigHandler::Handler(interrupt_handler);
    let action = SigAction::new(handler, SaFlags::empty(), SigSet::empty());
    unsafe {
        sigaction(Signal::SIGUSR1, &action)
            .map_err(|e| Error::StartFailed(format!("failed to install signal handler: {}", e)))?;
    }
    Ok(())
}

pub async fn start_vm(config: &VmConfig) -> Result<Box<dyn BackendVmHandle>> {
    let (kernel_path, initrd_path, cmdline) = match &config.boot {
        BootMethod::LinuxDirect {
            kernel,
            initrd,
            cmdline,
        } => (kernel.clone(), initrd.clone(), cmdline.clone()),
        BootMethod::Uefi { .. } => {
            // TODO: UEFI boot support
            return Err(Error::UnsupportedFeature(
                "UEFI boot not yet supported on KVM backend".into(),
            ));
        }
    };

    let cpus = config.resources.cpus;
    let memory_mb = config.resources.memory_mb as u64;
    let console_enabled = config.console_enabled;

    let (host_read, host_write, guest_read, serial) = if console_enabled {
        let (guest_read, host_write, host_read, guest_write) = create_console_pipes()
            .map_err(|e| Error::StartFailed(format!("failed to create console pipes: {}", e)))?;

        tracing::debug!("Console pipes created");
        let writer = ConsolePipeWriter(guest_write);
        let serial = Arc::new(SerialDevice::new(Box::new(writer)));
        (
            Some(host_read),
            Some(host_write),
            Some(guest_read),
            Some(serial),
        )
    } else {
        tracing::debug!("Console disabled");
        (None, None, None, None)
    };

    let kvm =
        Kvm::new().map_err(|e| Error::StartFailed(format!("failed to open /dev/kvm: {}", e)))?;
    let vm_fd = kvm
        .create_vm()
        .map_err(|e| Error::StartFailed(format!("failed to create VM: {}", e)))?;

    // Set up IRQ chip (PIC + IOAPIC) - required for x86_64
    vm_fd
        .create_irq_chip()
        .map_err(|e| Error::StartFailed(format!("failed to create IRQ chip: {}", e)))?;

    // Set up PIT (Programmable Interval Timer)
    let pit_config = kvm_pit_config::default();
    vm_fd
        .create_pit2(pit_config)
        .map_err(|e| Error::StartFailed(format!("failed to create PIT: {}", e)))?;

    let memory = create_guest_memory(memory_mb)
        .map_err(|e| Error::StartFailed(format!("failed to create guest memory: {}", e)))?;

    setup_memory_regions(&vm_fd, &memory)?;

    let (kernel_entry, kernel_header) = load_kernel(&memory, &kernel_path)?;
    tracing::debug!("Kernel loaded at entry point: 0x{:x}", kernel_entry);

    let initrd_addr = initrd_load_addr(kernel_entry);
    let initrd_size = load_initrd(&memory, &initrd_path, initrd_addr)?;
    tracing::debug!(
        "Initrd loaded at 0x{:x}, size: {} bytes",
        initrd_addr,
        initrd_size
    );

    tracing::debug!("Kernel cmdline: {}", cmdline);
    setup_boot_params(
        &memory,
        &cmdline,
        kernel_header,
        initrd_addr,
        initrd_size,
        memory_mb * 1024 * 1024,
    )
    .map_err(|e| Error::StartFailed(format!("failed to setup boot params: {}", e)))?;

    install_signal_handler()?;

    let running = Arc::new(AtomicBool::new(true));
    let (exit_tx, exit_rx) = mpsc::channel(1);

    let mut vcpu_handles = Vec::new();
    let mut vcpu_thread_ids = Vec::new();

    for vcpu_id in 0..cpus {
        let vcpu = vm_fd
            .create_vcpu(vcpu_id as u64)
            .map_err(|e| Error::StartFailed(format!("failed to create vCPU {}: {}", vcpu_id, e)))?;

        crate::arch::init_vcpu(&vcpu, &kvm)
            .map_err(|e| Error::StartFailed(format!("failed to init vCPU {}: {}", vcpu_id, e)))?;

        setup_sregs(&vcpu, memory_mb * 1024 * 1024)
            .map_err(|e| Error::StartFailed(format!("failed to setup vCPU sregs: {}", e)))?;

        if vcpu_id == 0 {
            setup_regs(&vcpu, kernel_entry, BOOT_PARAMS_ADDR).map_err(|e| {
                Error::StartFailed(format!("failed to setup vCPU registers: {}", e))
            })?;
        }

        let serial_clone = serial
            .clone()
            .unwrap_or_else(|| Arc::new(SerialDevice::new(Box::new(std::io::sink()))));
        let running_clone = running.clone();
        let exit_tx_clone = exit_tx.clone();

        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _ = tid_tx.send(nix::sys::pthread::pthread_self());
            run_vcpu(vcpu, serial_clone, running_clone, exit_tx_clone);
        });
        if let Ok(tid) = tid_rx.recv() {
            vcpu_thread_ids.push(tid);
        }
        vcpu_handles.push(handle);
    }

    let console_input_thread = if let (Some(serial), Some(guest_read)) = (&serial, guest_read) {
        let serial_clone = serial.clone();
        let running_clone = running.clone();

        // Set the pipe to non-blocking so we can check the running flag
        if let Err(e) = set_nonblocking(&guest_read) {
            tracing::warn!("failed to set console input pipe to non-blocking: {}", e);
        }

        Some(std::thread::spawn(move || {
            let mut reader = std::fs::File::from(guest_read);
            let mut buf = [0u8; 256];
            while running_clone.load(Ordering::Relaxed) {
                match reader.read(&mut buf) {
                    Ok(0) => break,
                    Ok(n) => {
                        serial_clone.enqueue_input(&buf[..n]);
                    }
                    Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => {
                        std::thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(_) => break,
                }
            }
        }))
    } else {
        None
    };

    Ok(Box::new(KvmVmHandle::new(
        running,
        exit_rx,
        vcpu_handles,
        vcpu_thread_ids,
        console_input_thread,
        host_read,
        host_write,
        console_enabled,
        memory,
    )))
}

fn setup_memory_regions(vm_fd: &VmFd, memory: &GuestMemoryMmap) -> Result<()> {
    for (index, region) in memory.iter().enumerate() {
        let mem_region = kvm_bindings::kvm_userspace_memory_region {
            slot: index as u32,
            guest_phys_addr: region.start_addr().raw_value(),
            memory_size: region.len(),
            userspace_addr: region.as_ptr() as u64,
            flags: 0,
        };
        unsafe { vm_fd.set_user_memory_region(mem_region) }
            .map_err(|e| Error::StartFailed(format!("failed to set memory region: {}", e)))?;
    }
    Ok(())
}

fn load_kernel(
    memory: &GuestMemoryMmap,
    kernel_path: &std::path::Path,
) -> Result<(u64, Option<linux_loader::bootparam::setup_header>)> {
    let mut kernel_file = File::open(kernel_path)
        .map_err(|e| Error::StartFailed(format!("failed to open kernel: {}", e)))?;

    let kernel_load_addr = GuestAddress(KERNEL_LOAD_ADDR);

    let result = BzImage::load(memory, Some(kernel_load_addr), &mut kernel_file, None)
        .map_err(|e| Error::StartFailed(format!("failed to load kernel: {:?}", e)))?;

    // For 64-bit boot, the entry point is kernel_load + 0x200
    let entry_64 = result.kernel_load.raw_value() + 0x200;

    Ok((entry_64, result.setup_header))
}

fn load_initrd(
    memory: &GuestMemoryMmap,
    initrd_path: &std::path::Path,
    load_addr: u64,
) -> Result<u64> {
    let mut initrd_file = File::open(initrd_path)
        .map_err(|e| Error::StartFailed(format!("failed to open initrd: {}", e)))?;

    let mut initrd_data = Vec::new();
    initrd_file
        .read_to_end(&mut initrd_data)
        .map_err(|e| Error::StartFailed(format!("failed to read initrd: {}", e)))?;

    memory
        .write_slice(&initrd_data, GuestAddress(load_addr))
        .map_err(|e| Error::StartFailed(format!("failed to write initrd to memory: {}", e)))?;

    Ok(initrd_data.len() as u64)
}

/// Adapter to write console output to a pipe file descriptor.
struct ConsolePipeWriter(OwnedFd);

impl Write for ConsolePipeWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        nix::unistd::write(&self.0, buf).map_err(|e| std::io::Error::from_raw_os_error(e as i32))
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
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
