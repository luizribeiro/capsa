use nix::libc;
use vm_memory::{GuestAddress, GuestMemoryMmap, GuestRegionMmap, MmapRegion};

pub const BOOT_GDT_OFFSET: u64 = 0x500;
pub const BOOT_IDT_OFFSET: u64 = 0x520;
pub const BOOT_STACK_POINTER: u64 = 0x8000;
pub const PML4_START: u64 = 0x9000;
pub const PDPTE_START: u64 = 0xa000;
pub const PDE_START: u64 = 0xb000;
pub const CMDLINE_OFFSET: u64 = 0x20000;
#[allow(dead_code)]
pub const CMDLINE_MAX_SIZE: u64 = 0x10000;
pub const KERNEL_LOAD_ADDR: u64 = 0x1000000; // 16MB - standard preferred address for 64-bit kernel
pub const INITRD_LOAD_ADDR: u64 = 0x4000000;
pub const BOOT_PARAMS_ADDR: u64 = 0x7000;

pub const MEM_START: u64 = 0;

pub fn create_guest_memory(memory_mb: u64) -> Result<GuestMemoryMmap, Box<dyn std::error::Error>> {
    let mem_size = memory_mb * 1024 * 1024;
    let mmap_region = MmapRegion::build(
        None,
        mem_size as usize,
        libc::PROT_READ | libc::PROT_WRITE,
        libc::MAP_ANONYMOUS | libc::MAP_PRIVATE,
    )?;
    let mem_region = GuestRegionMmap::new(mmap_region, GuestAddress(MEM_START))
        .ok_or("failed to create guest region")?;
    GuestMemoryMmap::from_regions(vec![mem_region])
        .map_err(|e| format!("failed to create guest memory: {}", e).into())
}

pub fn initrd_load_addr(_kernel_end: u64) -> u64 {
    INITRD_LOAD_ADDR
}
