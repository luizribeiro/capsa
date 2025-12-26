//! Virtio network device implementation.
//!
//! Provides network connectivity using virtio-net over MMIO transport.
//! Frames are exchanged via a socketpair, with one end going to the
//! UserNatStack for NAT processing.

use std::collections::VecDeque;
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use kvm_ioctls::VmFd;
use virtio_queue::desc::split::Descriptor;
use virtio_queue::{Queue, QueueT};
use vm_device::MutDeviceMmio;
use vm_device::bus::{MmioAddress, MmioAddressOffset};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

const VIRTIO_ID_NET: u32 = 1;

const RX_QUEUE_INDEX: usize = 0;
const TX_QUEUE_INDEX: usize = 1;
const QUEUE_SIZE: u16 = 256;

// Virtio MMIO register offsets
const VIRTIO_MMIO_MAGIC: u64 = 0x00;
const VIRTIO_MMIO_VERSION: u64 = 0x04;
const VIRTIO_MMIO_DEVICE_ID: u64 = 0x08;
const VIRTIO_MMIO_VENDOR_ID: u64 = 0x0c;
const VIRTIO_MMIO_DEVICE_FEATURES: u64 = 0x10;
const VIRTIO_MMIO_DEVICE_FEATURES_SEL: u64 = 0x14;
const VIRTIO_MMIO_DRIVER_FEATURES: u64 = 0x20;
const VIRTIO_MMIO_DRIVER_FEATURES_SEL: u64 = 0x24;
const VIRTIO_MMIO_QUEUE_SEL: u64 = 0x30;
const VIRTIO_MMIO_QUEUE_NUM_MAX: u64 = 0x34;
const VIRTIO_MMIO_QUEUE_NUM: u64 = 0x38;
const VIRTIO_MMIO_QUEUE_READY: u64 = 0x44;
const VIRTIO_MMIO_QUEUE_NOTIFY: u64 = 0x50;
const VIRTIO_MMIO_INTERRUPT_STATUS: u64 = 0x60;
const VIRTIO_MMIO_INTERRUPT_ACK: u64 = 0x64;
const VIRTIO_MMIO_STATUS: u64 = 0x70;
const VIRTIO_MMIO_QUEUE_DESC_LOW: u64 = 0x80;
const VIRTIO_MMIO_QUEUE_DESC_HIGH: u64 = 0x84;
const VIRTIO_MMIO_QUEUE_AVAIL_LOW: u64 = 0x90;
const VIRTIO_MMIO_QUEUE_AVAIL_HIGH: u64 = 0x94;
const VIRTIO_MMIO_QUEUE_USED_LOW: u64 = 0xa0;
const VIRTIO_MMIO_QUEUE_USED_HIGH: u64 = 0xa4;
const VIRTIO_MMIO_CONFIG: u64 = 0x100;

const VIRTIO_MMIO_MAGIC_VALUE: u32 = 0x74726976;

const VIRTIO_STATUS_DRIVER_OK: u32 = 4;
const VIRTIO_INT_USED_RING: u32 = 1;

// Virtio feature bits
const VIRTIO_F_VERSION_1: u64 = 1 << 32;
const VIRTIO_NET_F_MAC: u64 = 1 << 5;

// Virtio-net header size
// With VIRTIO_F_VERSION_1, the modern header includes num_buffers (12 bytes total)
const VIRTIO_NET_HDR_SIZE: usize = 12;

use super::{MAX_DESCRIPTOR_LEN, validate_queue_addresses};

struct VirtioQueueState {
    ready: bool,
    size: u16,
    desc_table: u64,
    avail_ring: u64,
    used_ring: u64,
    next_avail: u16,
    next_used: u16,
}

impl Default for VirtioQueueState {
    fn default() -> Self {
        Self {
            ready: false,
            size: QUEUE_SIZE,
            desc_table: 0,
            avail_ring: 0,
            used_ring: 0,
            next_avail: 0,
            next_used: 0,
        }
    }
}

/// Virtio network device using MMIO transport.
///
/// Uses a socketpair for frame I/O:
/// - Guest TX queue → write to socketpair → UserNatStack
/// - UserNatStack → write to socketpair → Guest RX queue
pub struct VirtioNet {
    device_features: u64,
    driver_features: u64,
    device_features_sel: u32,
    driver_features_sel: u32,
    device_status: u32,

    queue_sel: u32,
    queues: [VirtioQueueState; 2],

    interrupt_status: AtomicU32,

    /// VmFd for direct interrupt injection via set_irq_line
    vm_fd: Arc<VmFd>,
    /// IRQ number for this device
    irq: u32,

    /// File descriptor for frame I/O (our end of the socketpair)
    socket_fd: OwnedFd,

    /// MAC address for the device
    mac: [u8; 6],

    /// Pending frames received from the network (to be delivered to guest)
    rx_queue: VecDeque<Vec<u8>>,

    memory: Option<Arc<GuestMemoryMmap>>,
}

impl VirtioNet {
    /// Create a new virtio-net device.
    ///
    /// The `socket_fd` is our end of a socketpair; the other end should be
    /// passed to the UserNatStack via SocketPairDevice.
    /// The `vm_fd` is used for direct interrupt injection via set_irq_line.
    pub fn new(socket_fd: OwnedFd, mac: [u8; 6], vm_fd: Arc<VmFd>, irq: u32) -> Self {
        Self {
            device_features: VIRTIO_F_VERSION_1 | VIRTIO_NET_F_MAC,
            driver_features: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            device_status: 0,
            queue_sel: 0,
            queues: [VirtioQueueState::default(), VirtioQueueState::default()],
            interrupt_status: AtomicU32::new(0),
            vm_fd,
            irq,
            socket_fd,
            mac,
            rx_queue: VecDeque::new(),
            memory: None,
        }
    }

    pub fn set_memory(&mut self, memory: Arc<GuestMemoryMmap>) {
        self.memory = Some(memory);
    }

    fn signal_used_queue(&self) {
        self.interrupt_status
            .fetch_or(VIRTIO_INT_USED_RING, Ordering::SeqCst);
        // Edge-triggered interrupt: assert then de-assert
        let _ = self.vm_fd.set_irq_line(self.irq, true);
        let _ = self.vm_fd.set_irq_line(self.irq, false);
    }

    fn is_activated(&self) -> bool {
        self.device_status & VIRTIO_STATUS_DRIVER_OK != 0
    }

    fn current_queue(&self) -> &VirtioQueueState {
        &self.queues[self.queue_sel as usize]
    }

    fn current_queue_mut(&mut self) -> &mut VirtioQueueState {
        &mut self.queues[self.queue_sel as usize]
    }

    /// Try to receive frames from the socketpair (non-blocking).
    pub fn poll_rx(&mut self) {
        let mut buf = [0u8; 1514 + VIRTIO_NET_HDR_SIZE];
        loop {
            let n = unsafe {
                nix::libc::recv(
                    self.socket_fd.as_raw_fd(),
                    buf.as_mut_ptr() as *mut _,
                    buf.len(),
                    nix::libc::MSG_DONTWAIT,
                )
            };

            if n <= 0 {
                break;
            }

            // Frame received - add virtio-net header and queue it
            let mut frame_with_hdr = vec![0u8; VIRTIO_NET_HDR_SIZE + n as usize];
            frame_with_hdr[VIRTIO_NET_HDR_SIZE..].copy_from_slice(&buf[..n as usize]);
            self.rx_queue.push_back(frame_with_hdr);
        }

        // Try to deliver queued frames to guest
        if !self.rx_queue.is_empty() {
            self.process_rx_queue();
        }
    }

    /// Process TX queue: guest → network
    fn process_tx_queue(&mut self) {
        let memory = match &self.memory {
            Some(m) => m.as_ref(),
            None => return,
        };

        let queue_state = &self.queues[TX_QUEUE_INDEX];
        if !queue_state.ready {
            return;
        }

        let mut queue = Queue::new(queue_state.size).unwrap();
        let _ = queue.try_set_desc_table_address(GuestAddress(queue_state.desc_table));
        let _ = queue.try_set_avail_ring_address(GuestAddress(queue_state.avail_ring));
        let _ = queue.try_set_used_ring_address(GuestAddress(queue_state.used_ring));
        queue.set_next_avail(queue_state.next_avail);
        queue.set_next_used(queue_state.next_used);
        queue.set_ready(true);

        let mut used_any = false;

        while let Some(mut desc_chain) = queue.pop_descriptor_chain(memory) {
            let mut frame_data = Vec::new();

            for desc in desc_chain.by_ref() {
                let desc: Descriptor = desc;
                if desc.is_write_only() {
                    continue;
                }

                let capped_len = std::cmp::min(desc.len(), MAX_DESCRIPTOR_LEN) as usize;
                let mut buf = vec![0u8; capped_len];
                if memory.read_slice(&mut buf, desc.addr()).is_ok() {
                    frame_data.extend_from_slice(&buf);
                }
            }

            // Skip the virtio-net header, send only the ethernet frame
            if frame_data.len() > VIRTIO_NET_HDR_SIZE {
                let frame = &frame_data[VIRTIO_NET_HDR_SIZE..];
                unsafe {
                    nix::libc::send(
                        self.socket_fd.as_raw_fd(),
                        frame.as_ptr() as *const _,
                        frame.len(),
                        0,
                    )
                };
            }

            if queue.add_used(memory, desc_chain.head_index(), 0).is_ok() {
                used_any = true;
            }
        }

        // Save the updated queue state for next time
        self.queues[TX_QUEUE_INDEX].next_avail = queue.next_avail();
        self.queues[TX_QUEUE_INDEX].next_used = queue.next_used();

        if used_any {
            self.signal_used_queue();
        }
    }

    /// Process RX queue: network → guest
    fn process_rx_queue(&mut self) {
        let memory = match &self.memory {
            Some(m) => m.as_ref(),
            None => return,
        };

        let queue_state = &self.queues[RX_QUEUE_INDEX];
        if !queue_state.ready || self.rx_queue.is_empty() {
            return;
        }

        let mut queue = Queue::new(queue_state.size).unwrap();
        let _ = queue.try_set_desc_table_address(GuestAddress(queue_state.desc_table));
        let _ = queue.try_set_avail_ring_address(GuestAddress(queue_state.avail_ring));
        let _ = queue.try_set_used_ring_address(GuestAddress(queue_state.used_ring));
        queue.set_next_avail(queue_state.next_avail);
        queue.set_next_used(queue_state.next_used);
        queue.set_ready(true);

        let mut used_any = false;

        while !self.rx_queue.is_empty() {
            let Some(mut desc_chain) = queue.pop_descriptor_chain(memory) else {
                break;
            };
            let frame = self.rx_queue.pop_front().unwrap();

            let mut written = 0u32;
            let mut frame_offset = 0usize;

            for desc in desc_chain.by_ref() {
                let desc: Descriptor = desc;
                if !desc.is_write_only() {
                    continue;
                }

                let remaining = frame.len().saturating_sub(frame_offset);
                if remaining == 0 {
                    break;
                }

                let to_write = std::cmp::min(desc.len() as usize, remaining);
                if memory
                    .write_slice(&frame[frame_offset..frame_offset + to_write], desc.addr())
                    .is_ok()
                {
                    written += to_write as u32;
                    frame_offset += to_write;
                }
            }

            if queue
                .add_used(memory, desc_chain.head_index(), written)
                .is_ok()
            {
                used_any = true;
            }
        }

        // Save the updated queue state for next time
        self.queues[RX_QUEUE_INDEX].next_avail = queue.next_avail();
        self.queues[RX_QUEUE_INDEX].next_used = queue.next_used();

        if used_any {
            self.signal_used_queue();
        }
    }

    fn handle_mmio_read(&self, offset: u64, data: &mut [u8]) {
        let val: u32 = match offset {
            VIRTIO_MMIO_MAGIC => VIRTIO_MMIO_MAGIC_VALUE,
            VIRTIO_MMIO_VERSION => 2,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_ID_NET,
            VIRTIO_MMIO_VENDOR_ID => 0x554d4551,
            VIRTIO_MMIO_DEVICE_FEATURES => {
                if self.device_features_sel == 0 {
                    self.device_features as u32
                } else {
                    (self.device_features >> 32) as u32
                }
            }
            VIRTIO_MMIO_QUEUE_NUM_MAX => QUEUE_SIZE as u32,
            VIRTIO_MMIO_QUEUE_READY => u32::from(self.current_queue().ready),
            VIRTIO_MMIO_INTERRUPT_STATUS => self.interrupt_status.load(Ordering::SeqCst),
            VIRTIO_MMIO_STATUS => self.device_status,
            // Device config: MAC address at offset 0x100
            o if (VIRTIO_MMIO_CONFIG..VIRTIO_MMIO_CONFIG + 6).contains(&o) => {
                let idx = (o - VIRTIO_MMIO_CONFIG) as usize;
                if !data.is_empty() && idx < 6 {
                    data[0] = self.mac[idx];
                    return;
                }
                0
            }
            _ => 0,
        };

        if data.len() >= 4 {
            data[..4].copy_from_slice(&val.to_le_bytes());
        }
    }

    fn handle_mmio_write(&mut self, offset: u64, data: &[u8]) {
        if data.len() < 4 {
            return;
        }
        let val = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);

        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => self.device_features_sel = val,
            VIRTIO_MMIO_DRIVER_FEATURES => {
                if self.driver_features_sel == 0 {
                    self.driver_features = (self.driver_features & 0xffffffff00000000) | val as u64;
                } else {
                    self.driver_features =
                        (self.driver_features & 0x00000000ffffffff) | ((val as u64) << 32);
                }
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => self.driver_features_sel = val,
            VIRTIO_MMIO_QUEUE_SEL => {
                if val < 2 {
                    self.queue_sel = val;
                }
            }
            VIRTIO_MMIO_QUEUE_NUM => {
                self.current_queue_mut().size = val as u16;
            }
            VIRTIO_MMIO_QUEUE_READY => {
                if val == 1 {
                    let q = self.current_queue();
                    if let Some(ref memory) = self.memory
                        && !validate_queue_addresses(
                            memory,
                            q.desc_table,
                            q.avail_ring,
                            q.used_ring,
                            q.size,
                        )
                    {
                        return;
                    }
                }
                self.current_queue_mut().ready = val == 1;
            }
            VIRTIO_MMIO_QUEUE_NOTIFY => {
                if self.is_activated() {
                    if val == TX_QUEUE_INDEX as u32 {
                        self.process_tx_queue();
                    } else if val == RX_QUEUE_INDEX as u32 {
                        self.process_rx_queue();
                    }
                }
            }
            VIRTIO_MMIO_INTERRUPT_ACK => {
                self.interrupt_status.fetch_and(!val, Ordering::SeqCst);
            }
            VIRTIO_MMIO_STATUS => {
                if val == 0 {
                    self.device_status = 0;
                    self.queues = [VirtioQueueState::default(), VirtioQueueState::default()];
                } else {
                    self.device_status = val;
                }
            }
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                let q = self.current_queue_mut();
                q.desc_table = (q.desc_table & 0xffffffff00000000) | val as u64;
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                let q = self.current_queue_mut();
                q.desc_table = (q.desc_table & 0x00000000ffffffff) | ((val as u64) << 32);
            }
            VIRTIO_MMIO_QUEUE_AVAIL_LOW => {
                let q = self.current_queue_mut();
                q.avail_ring = (q.avail_ring & 0xffffffff00000000) | val as u64;
            }
            VIRTIO_MMIO_QUEUE_AVAIL_HIGH => {
                let q = self.current_queue_mut();
                q.avail_ring = (q.avail_ring & 0x00000000ffffffff) | ((val as u64) << 32);
            }
            VIRTIO_MMIO_QUEUE_USED_LOW => {
                let q = self.current_queue_mut();
                q.used_ring = (q.used_ring & 0xffffffff00000000) | val as u64;
            }
            VIRTIO_MMIO_QUEUE_USED_HIGH => {
                let q = self.current_queue_mut();
                q.used_ring = (q.used_ring & 0x00000000ffffffff) | ((val as u64) << 32);
            }
            _ => {}
        }
    }
}

impl MutDeviceMmio for VirtioNet {
    fn mmio_read(&mut self, _base: MmioAddress, offset: MmioAddressOffset, data: &mut [u8]) {
        self.handle_mmio_read(offset, data);
    }

    fn mmio_write(&mut self, _base: MmioAddress, offset: MmioAddressOffset, data: &[u8]) {
        self.handle_mmio_write(offset, data);
    }
}
