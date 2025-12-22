use kvm_bindings::{KVM_MAX_CPUID_ENTRIES, kvm_regs, kvm_segment};
use kvm_ioctls::{Kvm, VcpuExit, VcpuFd};
use nix::libc;
use nix::sys::signal::{SigSet, SigmaskHow, Signal, pthread_sigmask};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use tokio::sync::mpsc;
use vm_device::MutDevicePio;
use vm_device::bus::{PioAddress, PioAddressOffset};
use vm_device::device_manager::{IoManager, PioManager};

use super::memory::{BOOT_GDT_OFFSET, BOOT_IDT_OFFSET, BOOT_STACK_POINTER, PML4_START};

pub const RTC_INDEX_PORT: u16 = 0x70;

// RTC register addresses
const RTC_REG_SECONDS: u8 = 0x00;
const RTC_REG_MINUTES: u8 = 0x02;
const RTC_REG_HOURS: u8 = 0x04;
const RTC_REG_WEEKDAY: u8 = 0x06;
const RTC_REG_DAY: u8 = 0x07;
const RTC_REG_MONTH: u8 = 0x08;
const RTC_REG_YEAR: u8 = 0x09;
const RTC_REG_STATUS_A: u8 = 0x0a;
const RTC_REG_STATUS_B: u8 = 0x0b;
const RTC_REG_STATUS_C: u8 = 0x0c;
const RTC_REG_STATUS_D: u8 = 0x0d;
const RTC_REG_CENTURY: u8 = 0x32;

// Status register values
// Status A: DV2:DV1:DV0 = 010 (32.768kHz timebase), RS3:RS2:RS1:RS0 = 0110 (1024Hz)
const RTC_STATUS_A_VALUE: u8 = 0x26;
// Status B: 24-hour mode, BCD format, no interrupts enabled
const RTC_STATUS_B_VALUE: u8 = 0x02;
// Status C: no interrupt flags set
const RTC_STATUS_C_VALUE: u8 = 0x00;
// Status D: bit 7 = RTC valid/battery good
const RTC_STATUS_D_VALUE: u8 = 0x80;

// TODO: CMOS RAM backing store (128 bytes)
// The real MC146818 has 128 bytes of battery-backed RAM where BIOS stores settings.
// Not needed for direct Linux boot (skips BIOS), but required for UEFI boot or if
// guest software expects to read/write CMOS configuration. Currently unimplemented
// registers return 0.

// TODO: Extended memory registers (0x34-0x35, 0x5b-0x5d)
// These registers report RAM size above 16MB and 4GB to BIOS/firmware.
// Not needed for direct Linux boot since the kernel gets memory info from boot
// params, but required for BIOS/UEFI boot flows.

// TODO: Alarm registers (0x01, 0x03, 0x05)
// Used for wake-from-sleep or scheduled RTC wakeups. Value 0xFF acts as wildcard.
// Not needed for VMs that are always running. Would require timer infrastructure
// to trigger interrupts.

// TODO: Periodic interrupt support (Status Register A rate selection)
// The MC146818 can generate periodic interrupts at configurable rates (2Hz-8kHz).
// Not needed since Linux prefers TSC/HPET for high-frequency timers.

// TODO: 12-hour mode support
// We hardcode 24-hour mode in Status Register B. Linux uses 24-hour mode, but
// some legacy DOS/Windows software might expect 12-hour with AM/PM bit.

pub struct RtcDevice {
    index: AtomicU8,
}

impl RtcDevice {
    pub fn new() -> Self {
        Self {
            index: AtomicU8::new(0),
        }
    }

    fn read_register(&self, index: u8) -> u8 {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let (year, month, day, hour, min, sec, weekday) = Self::unix_to_datetime(now);

        match index {
            RTC_REG_SECONDS => Self::to_bcd(sec),
            RTC_REG_MINUTES => Self::to_bcd(min),
            RTC_REG_HOURS => Self::to_bcd(hour),
            RTC_REG_WEEKDAY => weekday,
            RTC_REG_DAY => Self::to_bcd(day),
            RTC_REG_MONTH => Self::to_bcd(month),
            RTC_REG_YEAR => Self::to_bcd((year % 100) as u8),
            RTC_REG_STATUS_A => RTC_STATUS_A_VALUE,
            RTC_REG_STATUS_B => RTC_STATUS_B_VALUE,
            RTC_REG_STATUS_C => RTC_STATUS_C_VALUE,
            RTC_REG_STATUS_D => RTC_STATUS_D_VALUE,
            RTC_REG_CENTURY => Self::to_bcd((year / 100) as u8),
            _ => 0,
        }
    }

    fn unix_to_datetime(timestamp: u64) -> (u32, u8, u8, u8, u8, u8, u8) {
        let sec = (timestamp % 60) as u8;
        let min = ((timestamp / 60) % 60) as u8;
        let hour = ((timestamp / 3600) % 24) as u8;

        let mut days = (timestamp / 86400) as i64;
        let weekday = ((days + 4) % 7 + 1) as u8; // Unix epoch was Thursday (day 4), RTC uses 1=Sunday

        let mut year = 1970i32;
        loop {
            let days_in_year = if Self::is_leap_year(year) { 366 } else { 365 };
            if days < days_in_year {
                break;
            }
            days -= days_in_year;
            year += 1;
        }

        let leap = Self::is_leap_year(year);
        let month_days: [i64; 12] = [
            31,
            if leap { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ];

        let mut month = 12u8; // Default to December if loop completes (shouldn't happen)
        for (i, &d) in month_days.iter().enumerate() {
            if days < d {
                month = (i + 1) as u8;
                break;
            }
            days -= d;
        }
        let day = (days + 1) as u8;

        debug_assert!(
            (1..=12).contains(&month),
            "invalid month calculated: {}",
            month
        );
        debug_assert!((1..=31).contains(&day), "invalid day calculated: {}", day);

        (year as u32, month, day, hour, min, sec, weekday)
    }

    fn is_leap_year(year: i32) -> bool {
        (year % 4 == 0 && year % 100 != 0) || (year % 400 == 0)
    }

    fn to_bcd(val: u8) -> u8 {
        ((val / 10) << 4) | (val % 10)
    }
}

impl MutDevicePio for RtcDevice {
    fn pio_read(&mut self, _base: PioAddress, offset: PioAddressOffset, data: &mut [u8]) {
        if !data.is_empty() {
            data[0] = if offset == 1 {
                let index = self.index.load(Ordering::Relaxed) & 0x7f;
                self.read_register(index)
            } else {
                0xff
            };
        }
    }

    fn pio_write(&mut self, _base: PioAddress, offset: PioAddressOffset, data: &[u8]) {
        if offset == 0 && !data.is_empty() {
            self.index.store(data[0], Ordering::Relaxed);
        }
    }
}

pub fn init_vcpu(vcpu: &VcpuFd, kvm: &Kvm) -> Result<(), kvm_ioctls::Error> {
    let cpuid = kvm.get_supported_cpuid(KVM_MAX_CPUID_ENTRIES)?;
    vcpu.set_cpuid2(&cpuid)?;
    Ok(())
}

pub fn setup_regs(
    vcpu: &VcpuFd,
    kernel_entry: u64,
    boot_params_addr: u64,
) -> Result<(), kvm_ioctls::Error> {
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

/// Runs the vCPU loop until the VM exits or is killed.
///
/// This function handles the main vCPU execution loop, processing I/O exits
/// and signaling termination via the `exit_tx` channel.
///
/// # Exit Codes
///
/// - `0`: Normal exit (HLT or Shutdown)
/// - `1`: Error during vCPU execution
/// - `-1`: Killed by external signal (running flag set to false)
///
/// # Multi-CPU Behavior
///
/// When running multiple vCPUs, each runs this loop independently. The exit
/// channel has capacity 1, so only the first vCPU to exit will have its code
/// recorded. Other vCPUs will detect the `running` flag is false and exit
/// with code `-1`.
pub fn run_vcpu(
    mut vcpu: VcpuFd,
    io_manager: Arc<IoManager>,
    running: Arc<AtomicBool>,
    exit_tx: mpsc::Sender<i32>,
) {
    // Unblock SIGUSR1 to allow graceful vCPU shutdown.
    // SIGUSR1 is sent by KvmVmHandle::kill() to interrupt blocking KVM_RUN calls.
    // If this fails, the vCPU may not respond to kill signals properly, but we
    // continue anyway since the running flag check provides a fallback mechanism.
    let mut sigusr1_set = SigSet::empty();
    sigusr1_set.add(Signal::SIGUSR1);
    if let Err(e) = pthread_sigmask(SigmaskHow::SIG_UNBLOCK, Some(&sigusr1_set), None) {
        tracing::error!(
            "failed to unblock SIGUSR1 in vCPU thread: {}. VM shutdown may hang.",
            e
        );
    }

    loop {
        if !running.load(Ordering::Relaxed) {
            let _ = exit_tx.try_send(-1);
            break;
        }

        match vcpu.run() {
            Ok(VcpuExit::Hlt) | Ok(VcpuExit::Shutdown) => {
                let _ = exit_tx.try_send(0);
                break;
            }
            Ok(VcpuExit::IoIn(port, data)) => {
                if let Err(e) = io_manager.pio_read(PioAddress(port), data) {
                    // Unhandled I/O reads return 0xFF (floating bus behavior).
                    // This is expected for hardware probing by guests.
                    tracing::trace!("unhandled PIO read from port 0x{:04x}: {:?}", port, e);
                    data.fill(0xff);
                }
            }
            Ok(VcpuExit::IoOut(port, data)) => {
                if let Err(e) = io_manager.pio_write(PioAddress(port), data) {
                    tracing::trace!("unhandled PIO write to port 0x{:04x}: {:?}", port, e);
                }
            }
            Ok(VcpuExit::MmioRead(_, data)) => data.fill(0),
            Ok(VcpuExit::MmioWrite(_, _)) | Ok(_) => {}
            Err(e) => {
                if e.errno() == libc::EAGAIN || e.errno() == libc::EINTR {
                    continue;
                }
                tracing::error!("vcpu run error: {}", e);
                let _ = exit_tx.try_send(1);
                break;
            }
        }
    }
    running.store(false, Ordering::Relaxed);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_unix_to_datetime_epoch() {
        let (year, month, day, hour, min, sec, weekday) = RtcDevice::unix_to_datetime(0);
        assert_eq!(year, 1970);
        assert_eq!(month, 1);
        assert_eq!(day, 1);
        assert_eq!(hour, 0);
        assert_eq!(min, 0);
        assert_eq!(sec, 0);
        assert_eq!(weekday, 5); // Thursday = day 5 (1=Sunday)
    }

    #[test]
    fn test_unix_to_datetime_known_date() {
        // 2024-02-29 12:30:45 UTC (leap day)
        let timestamp = 1709209845;
        let (year, month, day, hour, min, sec, _) = RtcDevice::unix_to_datetime(timestamp);
        assert_eq!(year, 2024);
        assert_eq!(month, 2);
        assert_eq!(day, 29);
        assert_eq!(hour, 12);
        assert_eq!(min, 30);
        assert_eq!(sec, 45);
    }

    #[test]
    fn test_unix_to_datetime_end_of_year() {
        // 2023-12-31 23:59:59 UTC
        let timestamp = 1704067199;
        let (year, month, day, hour, min, sec, _) = RtcDevice::unix_to_datetime(timestamp);
        assert_eq!(year, 2023);
        assert_eq!(month, 12);
        assert_eq!(day, 31);
        assert_eq!(hour, 23);
        assert_eq!(min, 59);
        assert_eq!(sec, 59);
    }

    #[test]
    fn test_unix_to_datetime_start_of_year() {
        // 2024-01-01 00:00:00 UTC
        let timestamp = 1704067200;
        let (year, month, day, hour, min, sec, _) = RtcDevice::unix_to_datetime(timestamp);
        assert_eq!(year, 2024);
        assert_eq!(month, 1);
        assert_eq!(day, 1);
        assert_eq!(hour, 0);
        assert_eq!(min, 0);
        assert_eq!(sec, 0);
    }

    #[test]
    fn test_is_leap_year() {
        assert!(!RtcDevice::is_leap_year(1900)); // Divisible by 100 but not 400
        assert!(RtcDevice::is_leap_year(2000)); // Divisible by 400
        assert!(RtcDevice::is_leap_year(2024)); // Divisible by 4
        assert!(!RtcDevice::is_leap_year(2023)); // Not divisible by 4
        assert!(!RtcDevice::is_leap_year(2100)); // Divisible by 100 but not 400
    }

    #[test]
    fn test_to_bcd() {
        assert_eq!(RtcDevice::to_bcd(0), 0x00);
        assert_eq!(RtcDevice::to_bcd(9), 0x09);
        assert_eq!(RtcDevice::to_bcd(10), 0x10);
        assert_eq!(RtcDevice::to_bcd(25), 0x25);
        assert_eq!(RtcDevice::to_bcd(59), 0x59);
        assert_eq!(RtcDevice::to_bcd(99), 0x99);
    }

    #[test]
    fn test_rtc_register_read() {
        let rtc = RtcDevice::new();

        // Status registers should return fixed values
        assert_eq!(rtc.read_register(RTC_REG_STATUS_A), RTC_STATUS_A_VALUE);
        assert_eq!(rtc.read_register(RTC_REG_STATUS_B), RTC_STATUS_B_VALUE);
        assert_eq!(rtc.read_register(RTC_REG_STATUS_C), RTC_STATUS_C_VALUE);
        assert_eq!(rtc.read_register(RTC_REG_STATUS_D), RTC_STATUS_D_VALUE);

        // Unknown registers return 0
        assert_eq!(rtc.read_register(0xFF), 0);
    }

    #[test]
    fn test_weekday_calculation() {
        // Verify weekday for known dates (1=Sunday, 7=Saturday)
        // 1970-01-01 was Thursday (5)
        let (_, _, _, _, _, _, weekday) = RtcDevice::unix_to_datetime(0);
        assert_eq!(weekday, 5);

        // 2024-01-07 was Sunday (1)
        let (_, _, _, _, _, _, weekday) = RtcDevice::unix_to_datetime(1704585600);
        assert_eq!(weekday, 1);

        // 2024-01-13 was Saturday (7)
        let (_, _, _, _, _, _, weekday) = RtcDevice::unix_to_datetime(1705104000);
        assert_eq!(weekday, 7);
    }
}
