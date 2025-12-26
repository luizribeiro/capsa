//! Shared constants and utilities for virtio device implementations.

use vm_memory::{GuestAddress, GuestMemory, GuestMemoryMmap};

// Virtio split queue structure sizes (from virtio 1.0 spec section 2.6)
// Descriptor: addr(8) + len(4) + flags(2) + next(2) = 16 bytes
const VIRTIO_DESC_SIZE: u64 = 16;
// Available ring header: flags(2) + idx(2) = 4 bytes
const VIRTIO_AVAIL_HEADER: u64 = 4;
// Available ring entry: descriptor index (u16) = 2 bytes
const VIRTIO_AVAIL_ENTRY: u64 = 2;
// Available ring footer: used_event (u16) = 2 bytes
const VIRTIO_AVAIL_FOOTER: u64 = 2;
// Used ring header: flags(2) + idx(2) = 4 bytes
const VIRTIO_USED_HEADER: u64 = 4;
// Used ring entry: id(4) + len(4) = 8 bytes
const VIRTIO_USED_ENTRY: u64 = 8;
// Used ring footer: avail_event (u16) = 2 bytes
const VIRTIO_USED_FOOTER: u64 = 2;

/// Validates that a virtio queue's addresses are within guest memory bounds.
///
/// When a guest sets QUEUE_READY=1, we must validate that the descriptor table,
/// available ring, and used ring addresses point to valid guest memory. Otherwise,
/// a malicious guest could set addresses outside guest RAM, potentially causing:
/// - Host crashes when accessing invalid memory
/// - Information disclosure if addresses happen to be valid host memory
///
/// Returns `true` if all addresses are valid, `false` otherwise.
pub fn validate_queue_addresses(
    memory: &GuestMemoryMmap,
    desc_table: u64,
    avail_ring: u64,
    used_ring: u64,
    queue_size: u16,
) -> bool {
    // Calculate required sizes for each queue structure.
    // All calculations are safe from overflow since queue_size is u16
    // and the largest multiplier is 16 (max result: 65535 * 16 = 1,048,560).
    let desc_table_size = (queue_size as u64) * VIRTIO_DESC_SIZE;
    let avail_ring_size =
        VIRTIO_AVAIL_HEADER + (queue_size as u64) * VIRTIO_AVAIL_ENTRY + VIRTIO_AVAIL_FOOTER;
    let used_ring_size =
        VIRTIO_USED_HEADER + (queue_size as u64) * VIRTIO_USED_ENTRY + VIRTIO_USED_FOOTER;

    // Descriptor table can be zero-sized when queue_size is 0
    let desc_valid = memory.address_in_range(GuestAddress(desc_table))
        && (desc_table_size == 0
            || memory.address_in_range(GuestAddress(desc_table + desc_table_size - 1)));

    // Available and used rings always have fixed overhead (6 bytes minimum)
    let avail_valid = memory.address_in_range(GuestAddress(avail_ring))
        && memory.address_in_range(GuestAddress(avail_ring + avail_ring_size - 1));

    let used_valid = memory.address_in_range(GuestAddress(used_ring))
        && memory.address_in_range(GuestAddress(used_ring + used_ring_size - 1));

    if !desc_valid || !avail_valid || !used_valid {
        tracing::warn!(
            "Invalid virtio queue addresses: desc={:#x} (valid={}), avail={:#x} (valid={}), used={:#x} (valid={})",
            desc_table,
            desc_valid,
            avail_ring,
            avail_valid,
            used_ring,
            used_valid
        );
        return false;
    }

    true
}

/// Maximum descriptor length to prevent guest-triggered memory exhaustion.
///
/// When processing virtio descriptor chains, guests can specify arbitrarily large
/// buffer sizes (up to u32::MAX = 4GB). Without validation, a malicious guest could
/// trigger massive host memory allocations causing denial of service.
///
/// This limit is set to 64KB (65536 bytes) which:
/// - Accommodates all legitimate traffic (network MTU ~1500, jumbo frames ~9000)
/// - Is generous for console I/O and vsock packets
/// - Limits per-descriptor allocation to prevent DoS
/// - Is a clean power-of-2 boundary
///
/// Even with a full queue (256 descriptors), this caps allocation at ~16MB per queue,
/// which is bounded and manageable.
pub const MAX_DESCRIPTOR_LEN: u32 = 65536;

/// Maximum number of concurrent vSock connections per guest.
///
/// Without this limit, a malicious guest could open unlimited connections,
/// exhausting host memory and file descriptors. Each connection:
/// - Allocates state in the device's HashMap
/// - May trigger a Unix socket connection on the host via the bridge
/// - Consumes resources in the bridge's connection tracking
///
/// 1024 connections is generous for legitimate use cases while preventing
/// resource exhaustion attacks.
pub const MAX_VSOCK_CONNECTIONS: usize = 1024;

#[cfg(test)]
mod tests {
    use super::*;
    use vm_memory::GuestMemoryMmap;

    fn create_test_memory(size: usize) -> GuestMemoryMmap {
        GuestMemoryMmap::from_ranges(&[(GuestAddress(0), size)]).unwrap()
    }

    #[test]
    fn valid_addresses_within_bounds() {
        let memory = create_test_memory(1024 * 1024);
        let queue_size = 256u16;

        assert!(validate_queue_addresses(
            &memory, 0x1000, 0x5000, 0x8000, queue_size
        ));
    }

    #[test]
    fn reject_desc_table_out_of_bounds() {
        let memory = create_test_memory(64 * 1024);
        let queue_size = 256u16;

        // desc_table size = 256 * 16 = 4096 bytes
        // Place at end of memory so it extends beyond
        let desc_table = (64 * 1024 - 2048) as u64;

        assert!(!validate_queue_addresses(
            &memory, desc_table, 0x1000, 0x2000, queue_size
        ));
    }

    #[test]
    fn reject_avail_ring_out_of_bounds() {
        let memory = create_test_memory(64 * 1024);
        let queue_size = 256u16;

        // Place avail_ring completely outside memory
        let avail_ring = 64 * 1024 + 100;

        assert!(!validate_queue_addresses(
            &memory, 0x1000, avail_ring, 0x2000, queue_size
        ));
    }

    #[test]
    fn reject_used_ring_out_of_bounds() {
        let memory = create_test_memory(64 * 1024);
        let queue_size = 256u16;

        // used_ring size = 4 + 256*8 + 2 = 2054 bytes
        // Place so it extends beyond memory
        let used_ring = (64 * 1024 - 1000) as u64;

        assert!(!validate_queue_addresses(
            &memory, 0x1000, 0x2000, used_ring, queue_size
        ));
    }

    #[test]
    fn zero_size_queue_is_valid() {
        let memory = create_test_memory(64 * 1024);

        assert!(validate_queue_addresses(&memory, 0x1000, 0x2000, 0x3000, 0));
    }

    #[test]
    fn address_at_exact_boundary_is_valid() {
        let memory = create_test_memory(64 * 1024);
        let queue_size = 16u16;

        // desc_table size = 16 * 16 = 256 bytes
        // Place so last byte is at memory_size - 1 (should be valid)
        let desc_table = (64 * 1024 - 256) as u64;

        assert!(validate_queue_addresses(
            &memory, desc_table, 0x1000, 0x2000, queue_size
        ));
    }

    #[test]
    fn address_one_byte_past_boundary_is_invalid() {
        let memory = create_test_memory(64 * 1024);
        let queue_size = 16u16;

        // desc_table size = 16 * 16 = 256 bytes
        // Place so last byte is at memory_size (one past end)
        let desc_table = (64 * 1024 - 255) as u64;

        assert!(!validate_queue_addresses(
            &memory, desc_table, 0x1000, 0x2000, queue_size
        ));
    }

    #[test]
    fn maximum_queue_size_with_large_memory() {
        let memory = create_test_memory(10 * 1024 * 1024);
        let queue_size = 32768u16; // virtio spec max

        assert!(validate_queue_addresses(
            &memory, 0x10000, 0x100000, 0x200000, queue_size
        ));
    }
}
