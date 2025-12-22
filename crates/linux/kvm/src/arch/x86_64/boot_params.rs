use super::memory::{
    BOOT_GDT_OFFSET, BOOT_PARAMS_ADDR, CMDLINE_OFFSET, PDE_START, PDPTE_START, PML4_START,
};
use linux_loader::bootparam::{boot_params, setup_header};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

pub fn setup_boot_params(
    memory: &GuestMemoryMmap,
    cmdline: &str,
    kernel_header: Option<setup_header>,
    initrd_addr: u64,
    initrd_size: u64,
    memory_size: u64,
) -> Result<(), Box<dyn std::error::Error>> {
    write_cmdline(memory, cmdline)?;
    setup_page_tables(memory)?;
    setup_gdt(memory)?;

    let mut params = boot_params::default();

    // Copy the kernel's setup_header if provided
    if let Some(hdr) = kernel_header {
        params.hdr = hdr;
    }

    // Override fields we need to set ourselves
    params.hdr.type_of_loader = 0xff;
    params.hdr.cmd_line_ptr = CMDLINE_OFFSET as u32;
    params.hdr.cmdline_size = cmdline.len() as u32;

    if initrd_size > 0 {
        params.hdr.ramdisk_image = initrd_addr as u32;
        params.hdr.ramdisk_size = initrd_size as u32;
    }

    let high_mem_start: u64 = 0x100000;
    let mem_below_4g = memory_size.min(0xc0000000);

    params.e820_entries = 4;
    params.e820_table[0] = linux_loader::bootparam::boot_e820_entry {
        addr: 0,
        size: 0x9fc00,
        type_: 1, // RAM
    };
    params.e820_table[1] = linux_loader::bootparam::boot_e820_entry {
        addr: 0x9fc00,
        size: 0x400,
        type_: 2, // Reserved
    };
    params.e820_table[2] = linux_loader::bootparam::boot_e820_entry {
        addr: 0xe8000,
        size: 0x18000,
        type_: 2, // Reserved (BIOS)
    };
    params.e820_table[3] = linux_loader::bootparam::boot_e820_entry {
        addr: high_mem_start,
        size: mem_below_4g - high_mem_start,
        type_: 1, // RAM
    };

    let params_bytes = unsafe {
        std::slice::from_raw_parts(
            &params as *const boot_params as *const u8,
            std::mem::size_of::<boot_params>(),
        )
    };
    memory.write_slice(params_bytes, GuestAddress(BOOT_PARAMS_ADDR))?;

    Ok(())
}

fn write_cmdline(
    memory: &GuestMemoryMmap,
    cmdline: &str,
) -> Result<(), vm_memory::GuestMemoryError> {
    let mut cmdline_bytes = cmdline.as_bytes().to_vec();
    cmdline_bytes.push(0);
    memory.write_slice(&cmdline_bytes, GuestAddress(CMDLINE_OFFSET))
}

fn setup_page_tables(memory: &GuestMemoryMmap) -> Result<(), vm_memory::GuestMemoryError> {
    memory.write_obj(PDPTE_START | 0x3, GuestAddress(PML4_START))?;
    memory.write_obj(PDE_START | 0x3, GuestAddress(PDPTE_START))?;

    for i in 0..512u64 {
        let entry = (i << 21) | 0x83;
        memory.write_obj(entry, GuestAddress(PDE_START + i * 8))?;
    }

    Ok(())
}

fn setup_gdt(memory: &GuestMemoryMmap) -> Result<(), vm_memory::GuestMemoryError> {
    let gdt: [u64; 4] = [
        0,                     // NULL
        0,                     // Unused
        0x00af_9a00_0000_ffff, // 64-bit code segment
        0x00cf_9200_0000_ffff, // 64-bit data segment
    ];

    for (i, entry) in gdt.iter().enumerate() {
        memory.write_obj(*entry, GuestAddress(BOOT_GDT_OFFSET + (i as u64) * 8))?;
    }

    Ok(())
}
