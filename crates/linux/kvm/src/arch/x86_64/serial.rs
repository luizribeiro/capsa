pub const SERIAL_PORT_BASE: u16 = 0x3f8;
pub const SERIAL_PORT_END: u16 = 0x3ff;
pub const SERIAL_IRQ: u32 = 4;

pub const VIRTIO_MMIO_BASE: u64 = 0xd000_0000;
pub const VIRTIO_MMIO_SIZE: u64 = 0x200;
pub const VIRTIO_CONSOLE_IRQ: u32 = 5;

pub const VIRTIO_NET_MMIO_BASE: u64 = 0xd000_0200;
pub const VIRTIO_NET_IRQ: u32 = 6;
