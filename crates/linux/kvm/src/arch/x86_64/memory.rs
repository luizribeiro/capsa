//! x86_64 guest memory layout and management.
//!
//! The guest physical memory layout follows the Linux boot protocol:
//!
//! ```text
//! Address       Size        Description
//! ─────────────────────────────────────────────────────────────
//! 0x0000_0500   0x20        Global Descriptor Table (GDT)
//! 0x0000_0520   0x20        Interrupt Descriptor Table (IDT)
//! 0x0000_7000   0x1000      Boot parameters (struct boot_params)
//! 0x0000_8000   -           Initial stack pointer
//! 0x0000_9000   0x1000      PML4 page table
//! 0x0000_A000   0x1000      PDPTE page table
//! 0x0000_B000   0x1000      PDE page table
//! 0x0002_0000   0x10000     Kernel command line
//! 0x0100_0000   -           Kernel load address (16 MB)
//! 0x0400_0000   -           Initrd load address (64 MB)
//! ```

use nix::libc;
use vm_memory::{GuestAddress, GuestMemoryMmap, GuestRegionMmap, MmapRegion};

/// GDT location in low memory (follows real-mode convention).
pub const BOOT_GDT_OFFSET: u64 = 0x500;

/// IDT location in low memory (follows real-mode convention).
pub const BOOT_IDT_OFFSET: u64 = 0x520;

/// Initial stack pointer for the boot CPU.
pub const BOOT_STACK_POINTER: u64 = 0x8000;

/// Page Map Level 4 (PML4) table address for 64-bit paging.
pub const PML4_START: u64 = 0x9000;

/// Page Directory Pointer Table Entry (PDPTE) address.
pub const PDPTE_START: u64 = 0xa000;

/// Page Directory Entry (PDE) address.
pub const PDE_START: u64 = 0xb000;

/// Kernel command line string address.
pub const CMDLINE_OFFSET: u64 = 0x20000;

/// Maximum kernel command line size (64 KB).
#[allow(dead_code)]
pub const CMDLINE_MAX_SIZE: u64 = 0x10000;

/// Kernel load address (16 MB, standard for 64-bit bzImage).
pub const KERNEL_LOAD_ADDR: u64 = 0x1000000;

/// Initrd load address (64 MB).
pub const INITRD_LOAD_ADDR: u64 = 0x4000000;

/// Boot parameters (struct boot_params) address.
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
