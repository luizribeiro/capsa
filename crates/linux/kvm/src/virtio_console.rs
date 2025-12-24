//! Virtio console device implementation.
//!
//! Provides a high-performance console using virtio queues instead of
//! legacy 8250 UART emulation. Uses MMIO transport for simplicity.

use std::collections::VecDeque;
use std::io::Write;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use virtio_queue::desc::split::Descriptor;
use virtio_queue::{Queue, QueueT};
use vm_device::MutDeviceMmio;
use vm_device::bus::{MmioAddress, MmioAddressOffset};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};
use vmm_sys_util::eventfd::EventFd;

// Virtio device type for console
const VIRTIO_ID_CONSOLE: u32 = 3;

// Queue indices
const RX_QUEUE_INDEX: usize = 0;
const TX_QUEUE_INDEX: usize = 1;

// Queue size (number of descriptors)
const QUEUE_SIZE: u16 = 256;

// Virtio MMIO register offsets (virtio 1.0+ modern layout)
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

// Virtio MMIO magic value
const VIRTIO_MMIO_MAGIC_VALUE: u32 = 0x74726976; // "virt"

// Virtio device status bits
const VIRTIO_STATUS_DRIVER_OK: u32 = 4;

// Interrupt status bits
const VIRTIO_INT_USED_RING: u32 = 1;

// Virtio feature bits
const VIRTIO_F_VERSION_1: u64 = 1 << 32;

/// State for a single virtio queue
struct VirtioQueueState {
    ready: bool,
    size: u16,
    desc_table: u64,
    avail_ring: u64,
    used_ring: u64,
}

impl Default for VirtioQueueState {
    fn default() -> Self {
        Self {
            ready: false,
            size: QUEUE_SIZE,
            desc_table: 0,
            avail_ring: 0,
            used_ring: 0,
        }
    }
}

/// Virtio console device using MMIO transport
pub struct VirtioConsole {
    // Device configuration
    device_features: u64,
    driver_features: u64,
    device_features_sel: u32,
    driver_features_sel: u32,
    device_status: u32,

    // Queue state
    queue_sel: u32,
    queues: [VirtioQueueState; 2],

    // Interrupt handling
    interrupt_status: AtomicU32,
    interrupt_evt: Arc<EventFd>,

    // Console I/O
    output: Mutex<Box<dyn Write + Send>>,
    input_buffer: Mutex<VecDeque<u8>>,

    // Guest memory (set after VM memory is created)
    memory: Option<Arc<GuestMemoryMmap>>,
}

impl VirtioConsole {
    pub fn new(output: Box<dyn Write + Send>) -> Self {
        Self {
            device_features: VIRTIO_F_VERSION_1, // Required for virtio 1.0+
            driver_features: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            device_status: 0,
            queue_sel: 0,
            queues: [VirtioQueueState::default(), VirtioQueueState::default()],
            interrupt_status: AtomicU32::new(0),
            interrupt_evt: Arc::new(EventFd::new(0).expect("failed to create eventfd")),
            output: Mutex::new(output),
            input_buffer: Mutex::new(VecDeque::new()),
            memory: None,
        }
    }

    pub fn set_memory(&mut self, memory: Arc<GuestMemoryMmap>) {
        self.memory = Some(memory);
    }

    pub fn interrupt_evt(&self) -> &EventFd {
        &self.interrupt_evt
    }

    fn signal_used_queue(&self) {
        self.interrupt_status
            .fetch_or(VIRTIO_INT_USED_RING, Ordering::SeqCst);
        let _ = self.interrupt_evt.write(1);
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

    /// Process the transmit queue (guest -> device)
    fn process_tx_queue(&self) {
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
        queue.set_ready(true);

        let mut output = self.output.lock().unwrap();
        let mut used_any = false;

        while let Some(mut desc_chain) = queue.pop_descriptor_chain(memory) {
            let mut len = 0u32;

            while let Some(desc) = desc_chain.next() {
                let desc: Descriptor = desc;
                if desc.is_write_only() {
                    continue;
                }

                let mut buf = vec![0u8; desc.len() as usize];
                if memory.read_slice(&mut buf, desc.addr()).is_ok() {
                    let _ = output.write_all(&buf);
                    len += desc.len();
                }
            }

            if queue.add_used(memory, desc_chain.head_index(), len).is_ok() {
                used_any = true;
            }
        }

        if used_any {
            let _ = output.flush();
            self.signal_used_queue();
        }
    }

    /// Process the receive queue (device -> guest)
    fn process_rx_queue(&self) {
        let memory = match &self.memory {
            Some(m) => m.as_ref(),
            None => return,
        };

        let queue_state = &self.queues[RX_QUEUE_INDEX];
        if !queue_state.ready {
            return;
        }

        let mut input = self.input_buffer.lock().unwrap();
        if input.is_empty() {
            return;
        }

        let mut queue = Queue::new(queue_state.size).unwrap();
        let _ = queue.try_set_desc_table_address(GuestAddress(queue_state.desc_table));
        let _ = queue.try_set_avail_ring_address(GuestAddress(queue_state.avail_ring));
        let _ = queue.try_set_used_ring_address(GuestAddress(queue_state.used_ring));
        queue.set_ready(true);

        let mut used_any = false;

        while let Some(mut desc_chain) = queue.pop_descriptor_chain(memory) {
            if input.is_empty() {
                break;
            }

            let mut written = 0u32;

            while let Some(desc) = desc_chain.next() {
                let desc: Descriptor = desc;
                if !desc.is_write_only() {
                    continue;
                }

                let to_write = std::cmp::min(desc.len() as usize, input.len());
                if to_write == 0 {
                    break;
                }

                let data: Vec<u8> = input.drain(..to_write).collect();
                if memory.write_slice(&data, desc.addr()).is_ok() {
                    written += data.len() as u32;
                }
            }

            if queue
                .add_used(memory, desc_chain.head_index(), written)
                .is_ok()
            {
                used_any = true;
            }
        }

        if used_any {
            self.signal_used_queue();
        }
    }

    fn handle_mmio_read(&self, offset: u64, data: &mut [u8]) {
        let val: u32 = match offset {
            VIRTIO_MMIO_MAGIC => VIRTIO_MMIO_MAGIC_VALUE,
            VIRTIO_MMIO_VERSION => 2, // virtio 1.0+
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_ID_CONSOLE,
            VIRTIO_MMIO_VENDOR_ID => 0x554d4551, // "QEMU" for compatibility
            VIRTIO_MMIO_DEVICE_FEATURES => {
                if self.device_features_sel == 0 {
                    self.device_features as u32
                } else {
                    (self.device_features >> 32) as u32
                }
            }
            VIRTIO_MMIO_QUEUE_NUM_MAX => QUEUE_SIZE as u32,
            VIRTIO_MMIO_QUEUE_READY => {
                if self.current_queue().ready {
                    1
                } else {
                    0
                }
            }
            VIRTIO_MMIO_INTERRUPT_STATUS => self.interrupt_status.load(Ordering::SeqCst),
            VIRTIO_MMIO_STATUS => self.device_status,
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

impl MutDeviceMmio for VirtioConsole {
    fn mmio_read(&mut self, _base: MmioAddress, offset: MmioAddressOffset, data: &mut [u8]) {
        self.handle_mmio_read(offset, data);
    }

    fn mmio_write(&mut self, _base: MmioAddress, offset: MmioAddressOffset, data: &[u8]) {
        self.handle_mmio_write(offset, data);
    }
}
