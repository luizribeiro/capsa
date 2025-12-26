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
