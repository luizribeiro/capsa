//! Shared constants and utilities for virtio device implementations.

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
