use crate::arch::{
    BOOT_PARAMS_ADDR, KERNEL_LOAD_ADDR, RTC_INDEX_PORT, RtcDevice, SERIAL_IRQ, SERIAL_PORT_BASE,
    SERIAL_PORT_END, create_guest_memory, initrd_load_addr, run_vcpu, setup_boot_params,
    setup_regs, setup_sregs,
};
use crate::handle::KvmVmHandle;
use crate::serial::{SerialDevice, create_console_pipes};
use capsa_core::{BackendVmHandle, BootMethod, Error, Result, VmConfig};
use kvm_bindings::kvm_pit_config;
use kvm_ioctls::{Kvm, VmFd};
use linux_loader::loader::KernelLoader;
use linux_loader::loader::bzimage::BzImage;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use vm_device::bus::{PioAddress, PioRange};
use vm_device::device_manager::{IoManager, PioManager};
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
        // Wrap in Arc<Mutex> for IoManager registration and sharing with console input task
        let serial = Arc::new(Mutex::new(SerialDevice::new(Box::new(writer))));
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

    // Get the serial interrupt eventfd and dup the vm_fd for the interrupt thread.
    //
    // TODO: Investigate why irqfd doesn't work for serial interrupts.
    // We bypass irqfd and manually inject interrupts using KVM_IRQ_LINE because
    // irqfd doesn't deliver interrupts reliably. We tried:
    // - Basic irqfd registration (KVM_IRQFD)
    // - irqfd with resample (KVM_IRQFD_FLAG_RESAMPLE) for level-triggered semantics
    // - Non-blocking eventfd (EFD_NONBLOCK)
    // All approaches register successfully and KVM reports irqfd capability, but
    // interrupts never reach the guest. Possibly related to PIC vs IOAPIC routing
    // after early boot (crosvm notes: "After very early boot, the PIC is switched
    // off and legacy interrupts handled by IOAPIC").
    let serial_interrupt_data = if let Some(ref serial) = serial {
        let evt = serial
            .lock()
            .unwrap()
            .interrupt_evt()
            .try_clone()
            .map_err(|e| Error::StartFailed(format!("failed to clone interrupt eventfd: {}", e)))?;
        // Duplicate the VM fd so the interrupt thread can use it
        let vm_fd_dup = nix::unistd::dup(vm_fd.as_raw_fd())
            .map_err(|e| Error::StartFailed(format!("failed to dup vm_fd: {}", e)))?;
        Some((evt, vm_fd_dup))
    } else {
        None
    };

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

    // Create I/O manager and register devices
    let mut io_manager = IoManager::new();

    // Register serial device (shared with console input task)
    let serial_for_io = serial
        .clone()
        .unwrap_or_else(|| Arc::new(Mutex::new(SerialDevice::new(Box::new(std::io::sink())))));
    register_pio_device(
        &mut io_manager,
        SERIAL_PORT_BASE,
        SERIAL_PORT_END - SERIAL_PORT_BASE + 1,
        serial_for_io,
        "serial device",
    )?;

    // Register RTC device
    register_pio_device(
        &mut io_manager,
        RTC_INDEX_PORT,
        2,
        Arc::new(Mutex::new(RtcDevice::new())),
        "RTC device",
    )?;

    let io_manager = Arc::new(io_manager);

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

        let io_manager_clone = io_manager.clone();
        let running_clone = running.clone();
        let exit_tx_clone = exit_tx.clone();

        let (tid_tx, tid_rx) = std::sync::mpsc::channel();
        let handle = std::thread::spawn(move || {
            let _ = tid_tx.send(nix::sys::pthread::pthread_self());
            run_vcpu(vcpu, io_manager_clone, running_clone, exit_tx_clone);
        });
        if let Ok(tid) = tid_rx.recv() {
            vcpu_thread_ids.push(tid);
        }
        vcpu_handles.push(handle);
    }

    // Spawn the interrupt handling thread that watches the serial eventfd
    // and injects interrupts using the duplicated VM fd
    let _interrupt_thread = if let Some((evt, vm_fd_dup)) = serial_interrupt_data {
        let running_clone = running.clone();
        Some(std::thread::spawn(move || {
            // KVM_IRQ_LINE ioctl structure
            #[repr(C)]
            struct KvmIrqLevel {
                irq: u32,
                level: u32,
            }
            // KVM_IRQ_LINE ioctl number: _IOW('k', 0x61, struct kvm_irq_level)
            // 'k' = 0xAE, size of kvm_irq_level = 8 bytes
            // _IOW = 0x40000000 | (size << 16) | (type << 8) | nr
            const KVM_IRQ_LINE: nix::libc::c_ulong = 0x4008_AE61; // _IOW('k', 0x61, 8)

            while running_clone.load(Ordering::Relaxed) {
                match evt.read() {
                    Ok(_) => {
                        // For level-triggered interrupts, we need to pulse the line:
                        // first deassert (level=0), then assert (level=1).
                        // This ensures the PIC sees a transition and generates a new interrupt.
                        let irq_low = KvmIrqLevel {
                            irq: SERIAL_IRQ,
                            level: 0,
                        };
                        unsafe { nix::libc::ioctl(vm_fd_dup, KVM_IRQ_LINE, &irq_low) };

                        let irq_high = KvmIrqLevel {
                            irq: SERIAL_IRQ,
                            level: 1,
                        };
                        unsafe { nix::libc::ioctl(vm_fd_dup, KVM_IRQ_LINE, &irq_high) };
                    }
                    Err(_) => break,
                }
            }
            // Close the duplicated fd
            unsafe {
                nix::libc::close(vm_fd_dup);
            }
        }))
    } else {
        None
    };

    let console_input_task = if let (Some(serial), Some(guest_read)) = (&serial, guest_read) {
        let serial_clone = serial.clone();
        let running_clone = running.clone();

        let reader = std::fs::File::from(guest_read);
        let async_fd = AsyncFd::with_interest(reader, Interest::READABLE)
            .map_err(|e| Error::StartFailed(format!("failed to create AsyncFd: {}", e)))?;

        Some(tokio::spawn(async move {
            // Buffer sized for typical terminal input bursts. 256 bytes handles
            // paste operations and escape sequences without excessive syscalls.
            let mut buf = [0u8; 256];
            while running_clone.load(Ordering::Relaxed) {
                let mut guard = match async_fd.readable().await {
                    Ok(guard) => guard,
                    Err(e) => {
                        tracing::debug!("console input: AsyncFd readable error: {}", e);
                        break;
                    }
                };

                match guard.try_io(|inner| {
                    nix::unistd::read(inner.get_ref().as_raw_fd(), &mut buf)
                        .map_err(|e| std::io::Error::from_raw_os_error(e as i32))
                }) {
                    Ok(Ok(0)) => {
                        tracing::debug!("console input: EOF received");
                        break;
                    }
                    Ok(Ok(n)) => {
                        if let Ok(serial) = serial_clone.lock() {
                            serial.enqueue_input(&buf[..n]);
                        }
                    }
                    Ok(Err(e)) => {
                        tracing::debug!("console input: read error: {}", e);
                        break;
                    }
                    Err(_would_block) => continue,
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
        console_input_task,
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

fn register_pio_device<D: vm_device::MutDevicePio + Send + 'static>(
    io_manager: &mut IoManager,
    base: u16,
    size: u16,
    device: Arc<Mutex<D>>,
    name: &str,
) -> Result<()> {
    let range = PioRange::new(PioAddress(base), size)
        .map_err(|e| Error::StartFailed(format!("failed to create {} PIO range: {:?}", name, e)))?;
    io_manager
        .register_pio(range, device)
        .map_err(|e| Error::StartFailed(format!("failed to register {}: {:?}", name, e)))?;
    Ok(())
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
