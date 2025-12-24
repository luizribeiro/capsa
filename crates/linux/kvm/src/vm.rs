use crate::arch::{
    BOOT_PARAMS_ADDR, KERNEL_LOAD_ADDR, RTC_INDEX_PORT, RtcDevice, SERIAL_IRQ, SERIAL_PORT_BASE,
    SERIAL_PORT_END, VIRTIO_CONSOLE_IRQ, VIRTIO_MMIO_BASE, VIRTIO_MMIO_SIZE, VIRTIO_NET_IRQ,
    VIRTIO_NET_MMIO_BASE, create_guest_memory, initrd_load_addr, run_vcpu, setup_boot_params,
    setup_regs, setup_sregs,
};
use crate::handle::KvmVmHandle;
use crate::serial::{SerialDevice, create_console_pipes};
use crate::virtio_console::VirtioConsole;
use crate::virtio_net::VirtioNet;
use capsa_core::{BackendVmHandle, BootMethod, Error, NetworkMode, Result, VmConfig};
use capsa_net::{SocketPairDevice, StackConfig, UserNatStack};
use kvm_bindings::kvm_pit_config;
use kvm_ioctls::{Kvm, VmFd};
use linux_loader::loader::KernelLoader;
use linux_loader::loader::bzimage::BzImage;
use nix::sys::signal::{SaFlags, SigAction, SigHandler, SigSet, Signal, sigaction};
use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::io::Interest;
use tokio::io::unix::AsyncFd;
use tokio::sync::mpsc;
use vm_device::bus::{MmioAddress, MmioRange, PioAddress, PioRange};
use vm_device::device_manager::{IoManager, MmioManager, PioManager};
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

    let (host_read, host_write, guest_read, serial, virtio_console_fd) = if console_enabled {
        let (guest_read, host_write, host_read, guest_write) = create_console_pipes()
            .map_err(|e| Error::StartFailed(format!("failed to create console pipes: {}", e)))?;

        tracing::debug!("Console pipes created");

        // Dup the guest_write fd for virtio-console before serial consumes it
        let virtio_fd = nix::unistd::dup(guest_write.as_raw_fd())
            .map(|fd| unsafe { OwnedFd::from_raw_fd(fd) })
            .map_err(|e| Error::StartFailed(format!("failed to dup console fd: {}", e)))?;

        let writer = ConsolePipeWriter(guest_write);
        // Wrap in Arc<Mutex> for IoManager registration and sharing with console input task
        let serial = Arc::new(Mutex::new(SerialDevice::new(Box::new(writer))));
        (
            Some(host_read),
            Some(host_write),
            Some(guest_read),
            Some(serial),
            Some(virtio_fd),
        )
    } else {
        tracing::debug!("Console disabled");
        (None, None, None, None, None)
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

    // Register serial interrupt using irqfd - KVM will inject IRQ when eventfd is signaled
    if let Some(ref serial) = serial {
        let evt = serial
            .lock()
            .unwrap()
            .interrupt_evt()
            .try_clone()
            .map_err(|e| Error::StartFailed(format!("failed to clone interrupt eventfd: {}", e)))?;
        vm_fd
            .register_irqfd(&evt, SERIAL_IRQ)
            .map_err(|e| Error::StartFailed(format!("failed to register serial irqfd: {}", e)))?;
    }

    // Set up PIT (Programmable Interval Timer)
    let pit_config = kvm_pit_config::default();
    vm_fd
        .create_pit2(pit_config)
        .map_err(|e| Error::StartFailed(format!("failed to create PIT: {}", e)))?;

    let memory = create_guest_memory(memory_mb)
        .map_err(|e| Error::StartFailed(format!("failed to create guest memory: {}", e)))?;
    let memory = Arc::new(memory);

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

    // Register virtio-console device if console is enabled
    if let Some(fd) = virtio_console_fd {
        let writer = ConsolePipeWriter(fd);
        let console = Arc::new(Mutex::new(VirtioConsole::new(Box::new(writer))));
        console.lock().unwrap().set_memory(memory.clone());

        // Register irqfd for virtio-console interrupts
        let evt = console
            .lock()
            .unwrap()
            .interrupt_evt()
            .try_clone()
            .map_err(|e| {
                Error::StartFailed(format!(
                    "failed to clone virtio-console interrupt fd: {}",
                    e
                ))
            })?;
        vm_fd
            .register_irqfd(&evt, VIRTIO_CONSOLE_IRQ)
            .map_err(|e| {
                Error::StartFailed(format!("failed to register virtio-console irqfd: {}", e))
            })?;

        register_mmio_device(
            &mut io_manager,
            VIRTIO_MMIO_BASE,
            VIRTIO_MMIO_SIZE,
            console,
            "virtio-console",
        )?;
    }

    // Set up virtio-net device if UserNat networking is configured
    let _network_task = if let NetworkMode::UserNat(_) = &config.network {
        // Create socketpair for frame I/O between virtio-net and UserNatStack
        let (host_device, guest_fd) = SocketPairDevice::new().map_err(|e| {
            Error::StartFailed(format!("failed to create network socketpair: {}", e))
        })?;

        // Default MAC address for the guest (52:54:00:xx:xx:xx is QEMU convention)
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];

        let virtio_net = Arc::new(Mutex::new(VirtioNet::new(guest_fd, mac)));
        virtio_net.lock().unwrap().set_memory(memory.clone());

        // Register irqfd for virtio-net interrupts
        let evt = virtio_net
            .lock()
            .unwrap()
            .interrupt_evt()
            .try_clone()
            .map_err(|e| {
                Error::StartFailed(format!("failed to clone virtio-net interrupt fd: {}", e))
            })?;
        vm_fd.register_irqfd(&evt, VIRTIO_NET_IRQ).map_err(|e| {
            Error::StartFailed(format!("failed to register virtio-net irqfd: {}", e))
        })?;

        register_mmio_device(
            &mut io_manager,
            VIRTIO_NET_MMIO_BASE,
            VIRTIO_MMIO_SIZE,
            virtio_net.clone(),
            "virtio-net",
        )?;

        tracing::debug!("virtio-net device registered");

        // Spawn the UserNatStack to handle NAT
        let stack = UserNatStack::new(host_device, StackConfig::default());
        tokio::spawn(async move {
            if let Err(e) = stack.run().await {
                tracing::error!("UserNat stack error: {:?}", e);
            }
        });

        // Spawn a task to poll for incoming frames from the network
        let virtio_net_for_rx = virtio_net.clone();
        Some(tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(1));
            loop {
                interval.tick().await;
                if let Ok(mut net) = virtio_net_for_rx.try_lock() {
                    net.poll_rx();
                }
            }
        }))
    } else {
        None
    };

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

fn register_mmio_device<D: vm_device::MutDeviceMmio + Send + 'static>(
    io_manager: &mut IoManager,
    base: u64,
    size: u64,
    device: Arc<Mutex<D>>,
    name: &str,
) -> Result<()> {
    let range = MmioRange::new(MmioAddress(base), size).map_err(|e| {
        Error::StartFailed(format!("failed to create {} MMIO range: {:?}", name, e))
    })?;
    io_manager
        .register_mmio(range, device)
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
