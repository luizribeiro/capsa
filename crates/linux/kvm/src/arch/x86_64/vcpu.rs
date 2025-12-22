use crate::serial::SerialDevice;
use kvm_bindings::{kvm_regs, kvm_segment, KVM_MAX_CPUID_ENTRIES};
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd};
use nix::libc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::Arc;
use tokio::sync::mpsc;

use super::memory::{BOOT_GDT_OFFSET, BOOT_IDT_OFFSET, BOOT_STACK_POINTER, PML4_START};

const RTC_INDEX_PORT: u16 = 0x70;
const RTC_DATA_PORT: u16 = 0x71;

pub struct RtcDevice {
    index: AtomicU8,
}

impl RtcDevice {
    pub fn new() -> Self {
        Self {
            index: AtomicU8::new(0),
        }
    }

    pub fn handles_io(&self, port: u16) -> bool {
        port == RTC_INDEX_PORT || port == RTC_DATA_PORT
    }

    pub fn io_read(&self, port: u16) -> u8 {
        if port == RTC_DATA_PORT {
            let index = self.index.load(Ordering::Relaxed) & 0x7f;
            self.read_register(index)
        } else {
            0xff
        }
    }

    pub fn io_write(&self, port: u16, data: u8) {
        if port == RTC_INDEX_PORT {
            self.index.store(data, Ordering::Relaxed);
        }
    }

    fn read_register(&self, index: u8) -> u8 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Convert to individual time components (simple UTC time)
        let secs = (now % 60) as u8;
        let mins = ((now / 60) % 60) as u8;
        let hours = ((now / 3600) % 24) as u8;

        match index {
            0x00 => self.to_bcd(secs),      // Seconds
            0x02 => self.to_bcd(mins),      // Minutes
            0x04 => self.to_bcd(hours),     // Hours
            0x06 => 1,                       // Day of week (1 = Sunday)
            0x07 => 22,                      // Day of month
            0x08 => 12,                      // Month
            0x09 => 25,                      // Year (2025 -> 25)
            0x0a => 0x26,                    // Status Register A (update not in progress)
            0x0b => 0x02,                    // Status Register B (24-hour mode, BCD)
            0x0c => 0x00,                    // Status Register C
            0x0d => 0x80,                    // Status Register D (RTC valid)
            0x32 => 0x20,                    // Century (20)
            _ => 0,
        }
    }

    fn to_bcd(&self, val: u8) -> u8 {
        ((val / 10) << 4) | (val % 10)
    }
}

pub fn init_vcpu(vcpu: &VcpuFd, kvm: &Kvm) -> Result<(), kvm_ioctls::Error> {
    let cpuid = kvm.get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)?;
    vcpu.set_cpuid2(&cpuid)?;
    Ok(())
}

pub fn setup_regs(vcpu: &VcpuFd, kernel_entry: u64, boot_params_addr: u64) -> Result<(), kvm_ioctls::Error> {
    let regs = kvm_regs {
        rflags: 0x2,
        rip: kernel_entry,
        rsp: BOOT_STACK_POINTER,
        rbp: BOOT_STACK_POINTER,
        rsi: boot_params_addr,
        ..Default::default()
    };
    vcpu.set_regs(&regs)?;
    Ok(())
}

pub fn setup_sregs(vcpu: &VcpuFd, _memory_size: u64) -> Result<(), kvm_ioctls::Error> {
    let mut sregs = vcpu.get_sregs()?;

    let code_seg = kvm_segment {
        base: 0,
        limit: 0xffffffff,
        selector: 0x10,
        type_: 0xb, // Execute/Read, accessed
        present: 1,
        dpl: 0,
        db: 0,
        s: 1,
        l: 1,
        g: 1,
        ..Default::default()
    };

    let data_seg = kvm_segment {
        base: 0,
        limit: 0xffffffff,
        selector: 0x18,
        type_: 0x3, // Read/Write, accessed
        present: 1,
        dpl: 0,
        db: 1,
        s: 1,
        l: 0,
        g: 1,
        ..Default::default()
    };

    sregs.cs = code_seg;
    sregs.ds = data_seg;
    sregs.es = data_seg;
    sregs.fs = data_seg;
    sregs.gs = data_seg;
    sregs.ss = data_seg;

    sregs.cr0 = 0x80050033; // PG, PE, WP, NE, ET, MP
    sregs.cr3 = PML4_START;
    sregs.cr4 = 0x668; // PAE, OSFXSR, OSXMMEXCPT, OSXSAVE
    sregs.efer = 0xd01; // LME, LMA, NXE, SCE

    sregs.gdt.base = BOOT_GDT_OFFSET;
    sregs.gdt.limit = 0x1f;
    sregs.idt.base = BOOT_IDT_OFFSET;
    sregs.idt.limit = 0xffff;

    vcpu.set_sregs(&sregs)?;
    Ok(())
}

pub fn run_vcpu(
    mut vcpu: VcpuFd,
    serial: Arc<SerialDevice>,
    running: Arc<AtomicBool>,
    exit_tx: mpsc::Sender<i32>,
) {
    let rtc = RtcDevice::new();

    loop {
        if !running.load(Ordering::Relaxed) {
            let _ = exit_tx.blocking_send(-1);
            break;
        }

        match vcpu.run() {
            Ok(VcpuExit::Hlt) => {
                let _ = exit_tx.blocking_send(0);
                break;
            }
            Ok(VcpuExit::Shutdown) => {
                let _ = exit_tx.blocking_send(0);
                break;
            }
            Ok(VcpuExit::IoIn(port, data)) => {
                if serial.handles_io(port) {
                    serial.io_read(port, data);
                } else if rtc.handles_io(port) {
                    if !data.is_empty() {
                        data[0] = rtc.io_read(port);
                    }
                } else {
                    data.fill(0xff);
                }
            }
            Ok(VcpuExit::IoOut(port, data)) => {
                if serial.handles_io(port) {
                    serial.io_write(port, data);
                } else if rtc.handles_io(port) {
                    if !data.is_empty() {
                        rtc.io_write(port, data[0]);
                    }
                }
            }
            Ok(VcpuExit::MmioRead(_, data)) => {
                data.fill(0);
            }
            Ok(VcpuExit::MmioWrite(_, _)) => {}
            Ok(_) => {}
            Err(e) => {
                if e.errno() == libc::EAGAIN || e.errno() == libc::EINTR {
                    continue;
                }
                tracing::error!("vcpu run error: {}", e);
                let _ = exit_tx.blocking_send(1);
                break;
            }
        }
    }
    running.store(false, Ordering::Relaxed);
}
