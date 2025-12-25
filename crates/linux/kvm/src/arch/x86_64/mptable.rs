//! MP table (Multi-Processor Specification) setup for x86_64.
//!
//! This module creates an MP table in guest memory that tells Linux about:
//! - CPU configuration and APIC IDs
//! - IOAPIC location and configuration
//! - Interrupt routing from ISA IRQs to IOAPIC pins
//!
//! Without the MP table, Linux cannot properly route interrupts through
//! the IOAPIC, which is required for virtio-mmio devices.

use vm_memory::{Address, Bytes, GuestAddress, GuestMemoryMmap};

/// MP Floating Pointer structure location in guest memory.
/// Placed in the EBDA area, near the end of conventional memory.
pub const MPTABLE_START: u64 = 0x9_fc00;

/// Maximum number of legacy IRQs to configure in the MP table.
const MAX_IRQ: u8 = 24;

/// IOAPIC default physical base address.
const IO_APIC_DEFAULT_PHYS_BASE: u32 = 0xfec0_0000;

/// LAPIC default physical base address.
const APIC_DEFAULT_PHYS_BASE: u32 = 0xfee0_0000;

/// APIC version (matches KVM's in-kernel APIC).
const APIC_VERSION: u8 = 0x14;

/// MP Specification version 1.4.
const MPC_SPEC: u8 = 4;

/// MP entry types.
const MP_PROCESSOR: u8 = 0;
const MP_BUS: u8 = 1;
const MP_IOAPIC: u8 = 2;
const MP_INTSRC: u8 = 3;
const MP_LINTSRC: u8 = 4;

/// CPU flags.
const CPU_ENABLED: u8 = 1;
const CPU_BOOTPROCESSOR: u8 = 2;

/// IOAPIC flags.
const MPC_APIC_USABLE: u8 = 1;

/// Interrupt types.
const MP_INT: u8 = 0;
const MP_EXTINT: u8 = 3;
const MP_NMI: u8 = 1;

/// IRQ polarity/trigger default.
const MP_IRQPOL_DEFAULT: u16 = 0;

/// Bus type ISA.
const BUS_TYPE_ISA: [u8; 6] = *b"ISA   ";

/// MP Floating Pointer structure.
#[repr(C, packed)]
#[derive(Default, Clone, Copy)]
struct MpfIntel {
    signature: [u8; 4],
    physptr: u32,
    length: u8,
    specification: u8,
    checksum: u8,
    feature1: u8,
    feature2: u8,
    feature3: u8,
    feature4: u8,
    feature5: u8,
}

/// MP Configuration Table header.
#[repr(C, packed)]
#[derive(Default, Clone, Copy)]
struct MpcTable {
    signature: [u8; 4],
    length: u16,
    spec: u8,
    checksum: u8,
    oem: [u8; 8],
    productid: [u8; 12],
    oemptr: u32,
    oemsize: u16,
    oemcount: u16,
    lapic: u32,
    reserved: u32,
}

/// MP Processor entry.
#[repr(C, packed)]
#[derive(Default, Clone, Copy)]
struct MpcCpu {
    type_: u8,
    apicid: u8,
    apicver: u8,
    cpuflag: u8,
    cpufeature: u32,
    featureflag: u32,
    reserved: [u32; 2],
}

/// MP Bus entry.
#[repr(C, packed)]
#[derive(Default, Clone, Copy)]
struct MpcBus {
    type_: u8,
    busid: u8,
    bustype: [u8; 6],
}

/// MP IOAPIC entry.
#[repr(C, packed)]
#[derive(Default, Clone, Copy)]
struct MpcIoapic {
    type_: u8,
    apicid: u8,
    apicver: u8,
    flags: u8,
    apicaddr: u32,
}

/// MP Interrupt Source entry.
#[repr(C, packed)]
#[derive(Default, Clone, Copy)]
struct MpcIntsrc {
    type_: u8,
    irqtype: u8,
    irqflag: u16,
    srcbus: u8,
    srcbusirq: u8,
    dstapic: u8,
    dstirq: u8,
}

/// MP Local Interrupt Source entry.
#[repr(C, packed)]
#[derive(Default, Clone, Copy)]
struct MpcLintsrc {
    type_: u8,
    irqtype: u8,
    irqflag: u16,
    srcbusid: u8,
    srcbusirq: u8,
    destapic: u8,
    destapiclint: u8,
}

fn compute_checksum<T>(data: &T) -> u8 {
    let bytes = unsafe {
        std::slice::from_raw_parts(data as *const T as *const u8, std::mem::size_of::<T>())
    };
    bytes.iter().fold(0u8, |acc, &b| acc.wrapping_add(b))
}

/// Sets up the MP table in guest memory.
///
/// The MP table includes:
/// - MP Floating Pointer structure
/// - MP Configuration Table with CPU, bus, IOAPIC, and interrupt entries
///
/// This is required for Linux to properly configure IOAPIC interrupt routing,
/// which is needed for virtio-mmio devices to receive interrupts.
pub fn setup_mptable(
    mem: &GuestMemoryMmap,
    num_cpus: u8,
) -> Result<(), Box<dyn std::error::Error>> {
    let mut base = GuestAddress(MPTABLE_START);
    let ioapic_id = num_cpus + 1;

    // Calculate total table size for checksum
    let config_table_size = std::mem::size_of::<MpcTable>()
        + std::mem::size_of::<MpcCpu>() * num_cpus as usize
        + std::mem::size_of::<MpcBus>()
        + std::mem::size_of::<MpcIoapic>()
        + std::mem::size_of::<MpcIntsrc>() * MAX_IRQ as usize
        + std::mem::size_of::<MpcLintsrc>() * 2;

    // Write MP Floating Pointer structure
    let mpf_size = std::mem::size_of::<MpfIntel>() as u64;
    let config_table_addr = base.0 + mpf_size;

    let mut mpf = MpfIntel {
        signature: *b"_MP_",
        physptr: config_table_addr as u32,
        length: 1,
        specification: MPC_SPEC,
        checksum: 0,
        ..Default::default()
    };
    mpf.checksum = (!compute_checksum(&mpf)).wrapping_add(1);

    let mpf_bytes =
        unsafe { std::slice::from_raw_parts(&mpf as *const _ as *const u8, mpf_size as usize) };
    mem.write_slice(mpf_bytes, base)?;
    base = base.unchecked_add(mpf_size);

    // Track position and checksum for config table entries
    let config_table_start = base;
    let mut checksum: u8 = 0;

    // Write MP Configuration Table header (we'll update length and checksum later)
    let mut mpc = MpcTable {
        signature: *b"PCMP",
        length: config_table_size as u16,
        spec: MPC_SPEC,
        checksum: 0,
        oem: *b"CAPSA   ",
        productid: *b"KVMVM       ",
        oemptr: 0,
        oemsize: 0,
        oemcount: (num_cpus as u16)
            + 1  // bus
            + 1  // ioapic
            + (MAX_IRQ as u16)  // interrupt sources
            + 2, // local interrupt sources
        lapic: APIC_DEFAULT_PHYS_BASE,
        reserved: 0,
    };
    checksum = checksum.wrapping_add(compute_checksum(&mpc));
    base = base.unchecked_add(std::mem::size_of::<MpcTable>() as u64);

    // Write CPU entries
    for cpu_id in 0..num_cpus {
        let cpu = MpcCpu {
            type_: MP_PROCESSOR,
            apicid: cpu_id,
            apicver: APIC_VERSION,
            cpuflag: CPU_ENABLED | if cpu_id == 0 { CPU_BOOTPROCESSOR } else { 0 },
            cpufeature: 0x600,  // CPU stepping
            featureflag: 0x201, // APIC + FPU
            reserved: [0; 2],
        };
        checksum = checksum.wrapping_add(compute_checksum(&cpu));

        let cpu_bytes = unsafe {
            std::slice::from_raw_parts(&cpu as *const _ as *const u8, std::mem::size_of::<MpcCpu>())
        };
        mem.write_slice(cpu_bytes, base)?;
        base = base.unchecked_add(std::mem::size_of::<MpcCpu>() as u64);
    }

    // Write Bus entry (ISA bus)
    let bus = MpcBus {
        type_: MP_BUS,
        busid: 0,
        bustype: BUS_TYPE_ISA,
    };
    checksum = checksum.wrapping_add(compute_checksum(&bus));

    let bus_bytes = unsafe {
        std::slice::from_raw_parts(&bus as *const _ as *const u8, std::mem::size_of::<MpcBus>())
    };
    mem.write_slice(bus_bytes, base)?;
    base = base.unchecked_add(std::mem::size_of::<MpcBus>() as u64);

    // Write IOAPIC entry
    let ioapic = MpcIoapic {
        type_: MP_IOAPIC,
        apicid: ioapic_id,
        apicver: APIC_VERSION,
        flags: MPC_APIC_USABLE,
        apicaddr: IO_APIC_DEFAULT_PHYS_BASE,
    };
    checksum = checksum.wrapping_add(compute_checksum(&ioapic));

    let ioapic_bytes = unsafe {
        std::slice::from_raw_parts(
            &ioapic as *const _ as *const u8,
            std::mem::size_of::<MpcIoapic>(),
        )
    };
    mem.write_slice(ioapic_bytes, base)?;
    base = base.unchecked_add(std::mem::size_of::<MpcIoapic>() as u64);

    // Write interrupt source entries for each IRQ
    for irq in 0..MAX_IRQ {
        let intsrc = MpcIntsrc {
            type_: MP_INTSRC,
            irqtype: MP_INT,
            irqflag: MP_IRQPOL_DEFAULT,
            srcbus: 0,
            srcbusirq: irq,
            dstapic: ioapic_id,
            dstirq: irq,
        };
        checksum = checksum.wrapping_add(compute_checksum(&intsrc));

        let intsrc_bytes = unsafe {
            std::slice::from_raw_parts(
                &intsrc as *const _ as *const u8,
                std::mem::size_of::<MpcIntsrc>(),
            )
        };
        mem.write_slice(intsrc_bytes, base)?;
        base = base.unchecked_add(std::mem::size_of::<MpcIntsrc>() as u64);
    }

    // Write local interrupt source entries (ExtINT and NMI)
    let lintsrc_extint = MpcLintsrc {
        type_: MP_LINTSRC,
        irqtype: MP_EXTINT,
        irqflag: 0,
        srcbusid: 0,
        srcbusirq: 0,
        destapic: 0xff, // All APICs
        destapiclint: 0,
    };
    checksum = checksum.wrapping_add(compute_checksum(&lintsrc_extint));

    let lintsrc_bytes = unsafe {
        std::slice::from_raw_parts(
            &lintsrc_extint as *const _ as *const u8,
            std::mem::size_of::<MpcLintsrc>(),
        )
    };
    mem.write_slice(lintsrc_bytes, base)?;
    base = base.unchecked_add(std::mem::size_of::<MpcLintsrc>() as u64);

    let lintsrc_nmi = MpcLintsrc {
        type_: MP_LINTSRC,
        irqtype: MP_NMI,
        irqflag: 0,
        srcbusid: 0,
        srcbusirq: 0,
        destapic: 0xff, // All APICs
        destapiclint: 1,
    };
    checksum = checksum.wrapping_add(compute_checksum(&lintsrc_nmi));

    let lintsrc_bytes = unsafe {
        std::slice::from_raw_parts(
            &lintsrc_nmi as *const _ as *const u8,
            std::mem::size_of::<MpcLintsrc>(),
        )
    };
    mem.write_slice(lintsrc_bytes, base)?;

    // Update the config table header with the correct checksum
    mpc.checksum = (!checksum).wrapping_add(1);
    let mpc_bytes = unsafe {
        std::slice::from_raw_parts(
            &mpc as *const _ as *const u8,
            std::mem::size_of::<MpcTable>(),
        )
    };
    mem.write_slice(mpc_bytes, config_table_start)?;

    Ok(())
}
