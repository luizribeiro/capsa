//! Common virtio MMIO constants and types shared across all devices.

/// Default queue size for virtio devices.
pub const DEFAULT_QUEUE_SIZE: u16 = 256;

// MMIO register offsets from the virtio 1.0 spec
pub const VIRTIO_MMIO_MAGIC: u64 = 0x00;
pub const VIRTIO_MMIO_VERSION: u64 = 0x04;
pub const VIRTIO_MMIO_DEVICE_ID: u64 = 0x08;
pub const VIRTIO_MMIO_VENDOR_ID: u64 = 0x0c;
pub const VIRTIO_MMIO_DEVICE_FEATURES: u64 = 0x10;
pub const VIRTIO_MMIO_DEVICE_FEATURES_SEL: u64 = 0x14;
pub const VIRTIO_MMIO_DRIVER_FEATURES: u64 = 0x20;
pub const VIRTIO_MMIO_DRIVER_FEATURES_SEL: u64 = 0x24;
pub const VIRTIO_MMIO_QUEUE_SEL: u64 = 0x30;
pub const VIRTIO_MMIO_QUEUE_NUM_MAX: u64 = 0x34;
pub const VIRTIO_MMIO_QUEUE_NUM: u64 = 0x38;
pub const VIRTIO_MMIO_QUEUE_READY: u64 = 0x44;
pub const VIRTIO_MMIO_QUEUE_NOTIFY: u64 = 0x50;
pub const VIRTIO_MMIO_INTERRUPT_STATUS: u64 = 0x60;
pub const VIRTIO_MMIO_INTERRUPT_ACK: u64 = 0x64;
pub const VIRTIO_MMIO_STATUS: u64 = 0x70;
pub const VIRTIO_MMIO_QUEUE_DESC_LOW: u64 = 0x80;
pub const VIRTIO_MMIO_QUEUE_DESC_HIGH: u64 = 0x84;
pub const VIRTIO_MMIO_QUEUE_AVAIL_LOW: u64 = 0x90;
pub const VIRTIO_MMIO_QUEUE_AVAIL_HIGH: u64 = 0x94;
pub const VIRTIO_MMIO_QUEUE_USED_LOW: u64 = 0xa0;
pub const VIRTIO_MMIO_QUEUE_USED_HIGH: u64 = 0xa4;
pub const VIRTIO_MMIO_CONFIG: u64 = 0x100;

/// Magic value for virtio MMIO devices ("virt" in little-endian).
pub const VIRTIO_MMIO_MAGIC_VALUE: u32 = 0x74726976;

/// Queue state shared by all virtio devices.
#[derive(Clone)]
pub struct VirtioQueueState {
    pub ready: bool,
    pub size: u16,
    pub desc_table: u64,
    pub avail_ring: u64,
    pub used_ring: u64,
    pub next_avail: u16,
    pub next_used: u16,
}

impl VirtioQueueState {
    pub fn new(size: u16) -> Self {
        Self {
            ready: false,
            size,
            desc_table: 0,
            avail_ring: 0,
            used_ring: 0,
            next_avail: 0,
            next_used: 0,
        }
    }
}

impl Default for VirtioQueueState {
    fn default() -> Self {
        Self::new(DEFAULT_QUEUE_SIZE)
    }
}
