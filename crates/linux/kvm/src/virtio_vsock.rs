//! Virtio vsock device implementation.
//!
//! Provides host-guest socket communication using virtio-vsock over MMIO transport.
//! Connections are bridged to Unix domain sockets on the host.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use kvm_ioctls::VmFd;
use tokio::sync::mpsc;
use virtio_queue::desc::split::Descriptor;
use virtio_queue::{Queue, QueueT};
use vm_device::MutDeviceMmio;
use vm_device::bus::{MmioAddress, MmioAddressOffset};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

const VIRTIO_ID_VSOCK: u32 = 19;

const RX_QUEUE_INDEX: usize = 0;
const TX_QUEUE_INDEX: usize = 1;
const QUEUE_SIZE: u16 = 256;
const NUM_QUEUES: usize = 3;

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

// Vsock header size (44 bytes)
const VSOCK_HDR_SIZE: usize = 44;

// Vsock operation codes
const VSOCK_OP_REQUEST: u16 = 1;
const VSOCK_OP_RESPONSE: u16 = 2;
const VSOCK_OP_RST: u16 = 3;
const VSOCK_OP_SHUTDOWN: u16 = 4;
const VSOCK_OP_RW: u16 = 5;
const VSOCK_OP_CREDIT_UPDATE: u16 = 6;
const VSOCK_OP_CREDIT_REQUEST: u16 = 7;

// Vsock type (stream only)
const VSOCK_TYPE_STREAM: u16 = 1;

// CID constants
const VSOCK_HOST_CID: u64 = 2;
const VSOCK_GUEST_CID: u64 = 3;

// Buffer allocation for credit-based flow control
const VSOCK_BUF_ALLOC: u32 = 64 * 1024;

/// Vsock packet header (44 bytes).
#[repr(C, packed)]
#[derive(Clone, Copy, Default)]
struct VsockHeader {
    src_cid: u64,
    dst_cid: u64,
    src_port: u32,
    dst_port: u32,
    len: u32,
    type_: u16,
    op: u16,
    flags: u32,
    buf_alloc: u32,
    fwd_cnt: u32,
}

impl VsockHeader {
    fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < VSOCK_HDR_SIZE {
            return None;
        }
        Some(Self {
            src_cid: u64::from_le_bytes(data[0..8].try_into().ok()?),
            dst_cid: u64::from_le_bytes(data[8..16].try_into().ok()?),
            src_port: u32::from_le_bytes(data[16..20].try_into().ok()?),
            dst_port: u32::from_le_bytes(data[20..24].try_into().ok()?),
            len: u32::from_le_bytes(data[24..28].try_into().ok()?),
            type_: u16::from_le_bytes(data[28..30].try_into().ok()?),
            op: u16::from_le_bytes(data[30..32].try_into().ok()?),
            flags: u32::from_le_bytes(data[32..36].try_into().ok()?),
            buf_alloc: u32::from_le_bytes(data[36..40].try_into().ok()?),
            fwd_cnt: u32::from_le_bytes(data[40..44].try_into().ok()?),
        })
    }

    fn to_bytes(self) -> [u8; VSOCK_HDR_SIZE] {
        let mut buf = [0u8; VSOCK_HDR_SIZE];
        buf[0..8].copy_from_slice(&self.src_cid.to_le_bytes());
        buf[8..16].copy_from_slice(&self.dst_cid.to_le_bytes());
        buf[16..20].copy_from_slice(&self.src_port.to_le_bytes());
        buf[20..24].copy_from_slice(&self.dst_port.to_le_bytes());
        buf[24..28].copy_from_slice(&self.len.to_le_bytes());
        buf[28..30].copy_from_slice(&self.type_.to_le_bytes());
        buf[30..32].copy_from_slice(&self.op.to_le_bytes());
        buf[32..36].copy_from_slice(&self.flags.to_le_bytes());
        buf[36..40].copy_from_slice(&self.buf_alloc.to_le_bytes());
        buf[40..44].copy_from_slice(&self.fwd_cnt.to_le_bytes());
        buf
    }
}

/// Connection key for tracking vsock connections.
#[derive(Clone, Copy, PartialEq, Eq, Hash, Debug)]
struct ConnKey {
    local_port: u32,
    peer_port: u32,
}

/// State of a vsock connection.
struct VsockConnection {
    /// Credit-based flow control: bytes we've forwarded to peer
    fwd_cnt: u32,
    /// Peer's buf_alloc
    peer_buf_alloc: u32,
}

/// Message from bridge to device.
pub enum BridgeToDevice {
    /// Data received from Unix socket to send to guest
    Data { local_port: u32, data: Vec<u8> },
    /// Connection closed from host side
    Closed { local_port: u32 },
}

/// Message from device to bridge.
pub enum DeviceToBridge {
    /// New connection request from guest
    Connect { local_port: u32 },
    /// Data from guest to send to Unix socket
    Data { local_port: u32, data: Vec<u8> },
    /// Connection closed from guest side
    Shutdown { local_port: u32 },
}

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

/// Virtio vsock device using MMIO transport.
pub struct VirtioVsock {
    device_features: u64,
    driver_features: u64,
    device_features_sel: u32,
    driver_features_sel: u32,
    device_status: u32,

    queue_sel: u32,
    queues: [VirtioQueueState; NUM_QUEUES],

    interrupt_status: AtomicU32,

    vm_fd: Arc<VmFd>,
    irq: u32,

    /// Guest CID (reported in config space)
    guest_cid: u64,

    /// Active connections
    connections: HashMap<ConnKey, VsockConnection>,

    /// Packets queued to send to guest (RX queue)
    rx_queue: std::collections::VecDeque<Vec<u8>>,

    /// Channel to send messages to bridge
    bridge_tx: mpsc::UnboundedSender<DeviceToBridge>,

    /// Channel to receive messages from bridge
    bridge_rx: mpsc::UnboundedReceiver<BridgeToDevice>,

    memory: Option<Arc<GuestMemoryMmap>>,
}

impl VirtioVsock {
    pub fn new(
        vm_fd: Arc<VmFd>,
        irq: u32,
        bridge_tx: mpsc::UnboundedSender<DeviceToBridge>,
        bridge_rx: mpsc::UnboundedReceiver<BridgeToDevice>,
    ) -> Self {
        Self {
            device_features: VIRTIO_F_VERSION_1,
            driver_features: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            device_status: 0,
            queue_sel: 0,
            queues: [
                VirtioQueueState::default(),
                VirtioQueueState::default(),
                VirtioQueueState::default(),
            ],
            interrupt_status: AtomicU32::new(0),
            vm_fd,
            irq,
            guest_cid: VSOCK_GUEST_CID,
            connections: HashMap::new(),
            rx_queue: std::collections::VecDeque::new(),
            bridge_tx,
            bridge_rx,
            memory: None,
        }
    }

    pub fn set_memory(&mut self, memory: Arc<GuestMemoryMmap>) {
        self.memory = Some(memory);
    }

    fn signal_used_queue(&self) {
        self.interrupt_status
            .fetch_or(VIRTIO_INT_USED_RING, Ordering::SeqCst);
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

    /// Poll for messages from bridge and queue responses.
    pub fn poll(&mut self) {
        while let Ok(msg) = self.bridge_rx.try_recv() {
            match msg {
                BridgeToDevice::Data { local_port, data } => {
                    self.handle_bridge_data(local_port, data);
                }
                BridgeToDevice::Closed { local_port } => {
                    self.handle_bridge_closed(local_port);
                }
            }
        }

        if !self.rx_queue.is_empty() {
            self.process_rx_queue();
        }
    }

    fn handle_bridge_data(&mut self, local_port: u32, data: Vec<u8>) {
        // Find the connection for this port
        let key = self
            .connections
            .keys()
            .find(|k| k.local_port == local_port)
            .copied();

        if let Some(key) = key {
            let fwd_cnt = self.connections.get(&key).map(|c| c.fwd_cnt).unwrap_or(0);

            // Build RW packet to send data to guest
            let hdr = VsockHeader {
                src_cid: VSOCK_HOST_CID,
                dst_cid: VSOCK_GUEST_CID,
                src_port: key.local_port,
                dst_port: key.peer_port,
                len: data.len() as u32,
                type_: VSOCK_TYPE_STREAM,
                op: VSOCK_OP_RW,
                buf_alloc: VSOCK_BUF_ALLOC,
                fwd_cnt,
                ..Default::default()
            };

            let mut packet = hdr.to_bytes().to_vec();
            packet.extend_from_slice(&data);
            self.rx_queue.push_back(packet);
        }
    }

    fn handle_bridge_closed(&mut self, local_port: u32) {
        // Find and remove the connection
        let key = self
            .connections
            .keys()
            .find(|k| k.local_port == local_port)
            .copied();

        if let Some(key) = key {
            self.connections.remove(&key);

            // Send RST to guest
            let hdr = VsockHeader {
                src_cid: VSOCK_HOST_CID,
                dst_cid: VSOCK_GUEST_CID,
                src_port: key.local_port,
                dst_port: key.peer_port,
                type_: VSOCK_TYPE_STREAM,
                op: VSOCK_OP_RST,
                ..Default::default()
            };

            self.rx_queue.push_back(hdr.to_bytes().to_vec());
        }
    }

    /// Process TX queue: guest → host
    fn process_tx_queue(&mut self) {
        let memory = match &self.memory {
            Some(m) => m.clone(),
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
        let mut packets_to_process = Vec::new();

        while let Some(mut desc_chain) = queue.pop_descriptor_chain(memory.as_ref()) {
            let mut packet_data = Vec::new();

            for desc in desc_chain.by_ref() {
                let desc: Descriptor = desc;
                if desc.is_write_only() {
                    continue;
                }

                let mut buf = vec![0u8; desc.len() as usize];
                if memory.read_slice(&mut buf, desc.addr()).is_ok() {
                    packet_data.extend_from_slice(&buf);
                }
            }

            if packet_data.len() >= VSOCK_HDR_SIZE {
                packets_to_process.push(packet_data);
            }

            if queue
                .add_used(memory.as_ref(), desc_chain.head_index(), 0)
                .is_ok()
            {
                used_any = true;
            }
        }

        self.queues[TX_QUEUE_INDEX].next_avail = queue.next_avail();
        self.queues[TX_QUEUE_INDEX].next_used = queue.next_used();

        // Process packets after releasing the queue borrow
        for packet_data in packets_to_process {
            self.handle_tx_packet(&packet_data);
        }

        if used_any {
            self.signal_used_queue();
        }
    }

    fn handle_tx_packet(&mut self, packet_data: &[u8]) {
        let Some(hdr) = VsockHeader::from_bytes(packet_data) else {
            return;
        };

        // Validate destination is host
        if hdr.dst_cid != VSOCK_HOST_CID {
            return;
        }

        match hdr.op {
            VSOCK_OP_REQUEST => self.handle_connect_request(&hdr),
            VSOCK_OP_RW => self.handle_rw_packet(&hdr, packet_data),
            VSOCK_OP_SHUTDOWN => self.handle_shutdown(&hdr),
            VSOCK_OP_RST => self.handle_rst(&hdr),
            VSOCK_OP_CREDIT_UPDATE => self.handle_credit_update(&hdr),
            VSOCK_OP_CREDIT_REQUEST => self.handle_credit_request(&hdr),
            _ => {}
        }
    }

    fn handle_connect_request(&mut self, hdr: &VsockHeader) {
        let key = ConnKey {
            local_port: hdr.dst_port,
            peer_port: hdr.src_port,
        };

        // Notify bridge of new connection
        let _ = self.bridge_tx.send(DeviceToBridge::Connect {
            local_port: hdr.dst_port,
        });

        // Create connection state
        let conn = VsockConnection {
            fwd_cnt: 0,
            peer_buf_alloc: hdr.buf_alloc,
        };
        self.connections.insert(key, conn);

        // Send RESPONSE back to guest
        let resp = VsockHeader {
            src_cid: VSOCK_HOST_CID,
            dst_cid: VSOCK_GUEST_CID,
            src_port: hdr.dst_port,
            dst_port: hdr.src_port,
            type_: VSOCK_TYPE_STREAM,
            op: VSOCK_OP_RESPONSE,
            buf_alloc: VSOCK_BUF_ALLOC,
            fwd_cnt: 0,
            ..Default::default()
        };

        self.rx_queue.push_back(resp.to_bytes().to_vec());
    }

    fn handle_rw_packet(&mut self, hdr: &VsockHeader, packet_data: &[u8]) {
        let key = ConnKey {
            local_port: hdr.dst_port,
            peer_port: hdr.src_port,
        };

        if let Some(conn) = self.connections.get_mut(&key) {
            // Update peer's credit info
            conn.peer_buf_alloc = hdr.buf_alloc;

            // Extract data and send to bridge
            let data_len = hdr.len as usize;
            if packet_data.len() >= VSOCK_HDR_SIZE + data_len {
                let data = packet_data[VSOCK_HDR_SIZE..VSOCK_HDR_SIZE + data_len].to_vec();
                conn.fwd_cnt = conn.fwd_cnt.wrapping_add(data_len as u32);

                let _ = self.bridge_tx.send(DeviceToBridge::Data {
                    local_port: hdr.dst_port,
                    data,
                });
            }
        }
    }

    fn handle_shutdown(&mut self, hdr: &VsockHeader) {
        let key = ConnKey {
            local_port: hdr.dst_port,
            peer_port: hdr.src_port,
        };

        if self.connections.remove(&key).is_some() {
            let _ = self.bridge_tx.send(DeviceToBridge::Shutdown {
                local_port: hdr.dst_port,
            });

            // Send RST back to confirm
            let rst = VsockHeader {
                src_cid: VSOCK_HOST_CID,
                dst_cid: VSOCK_GUEST_CID,
                src_port: hdr.dst_port,
                dst_port: hdr.src_port,
                type_: VSOCK_TYPE_STREAM,
                op: VSOCK_OP_RST,
                ..Default::default()
            };

            self.rx_queue.push_back(rst.to_bytes().to_vec());
        }
    }

    fn handle_rst(&mut self, hdr: &VsockHeader) {
        let key = ConnKey {
            local_port: hdr.dst_port,
            peer_port: hdr.src_port,
        };

        if self.connections.remove(&key).is_some() {
            let _ = self.bridge_tx.send(DeviceToBridge::Shutdown {
                local_port: hdr.dst_port,
            });
        }
    }

    fn handle_credit_update(&mut self, hdr: &VsockHeader) {
        let key = ConnKey {
            local_port: hdr.dst_port,
            peer_port: hdr.src_port,
        };

        if let Some(conn) = self.connections.get_mut(&key) {
            conn.peer_buf_alloc = hdr.buf_alloc;
        }
    }

    fn handle_credit_request(&mut self, hdr: &VsockHeader) {
        let key = ConnKey {
            local_port: hdr.dst_port,
            peer_port: hdr.src_port,
        };

        if let Some(conn) = self.connections.get(&key) {
            // Send credit update
            let update = VsockHeader {
                src_cid: VSOCK_HOST_CID,
                dst_cid: VSOCK_GUEST_CID,
                src_port: hdr.dst_port,
                dst_port: hdr.src_port,
                type_: VSOCK_TYPE_STREAM,
                op: VSOCK_OP_CREDIT_UPDATE,
                buf_alloc: VSOCK_BUF_ALLOC,
                fwd_cnt: conn.fwd_cnt,
                ..Default::default()
            };

            self.rx_queue.push_back(update.to_bytes().to_vec());
        }
    }

    /// Process RX queue: host → guest
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
            let packet = self.rx_queue.pop_front().unwrap();

            let mut written = 0u32;
            let mut offset = 0usize;

            for desc in desc_chain.by_ref() {
                let desc: Descriptor = desc;
                if !desc.is_write_only() {
                    continue;
                }

                let remaining = packet.len().saturating_sub(offset);
                if remaining == 0 {
                    break;
                }

                let to_write = std::cmp::min(desc.len() as usize, remaining);
                if memory
                    .write_slice(&packet[offset..offset + to_write], desc.addr())
                    .is_ok()
                {
                    written += to_write as u32;
                    offset += to_write;
                }
            }

            if queue
                .add_used(memory, desc_chain.head_index(), written)
                .is_ok()
            {
                used_any = true;
            }
        }

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
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_ID_VSOCK,
            VIRTIO_MMIO_VENDOR_ID => 0x554d4551, // "QEMU"
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
            // Config space: guest_cid (8 bytes at offset 0x100)
            VIRTIO_MMIO_CONFIG => self.guest_cid as u32,
            o if o == VIRTIO_MMIO_CONFIG + 4 => (self.guest_cid >> 32) as u32,
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
                if val < NUM_QUEUES as u32 {
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
                    // EVENT_QUEUE not used for now
                }
            }
            VIRTIO_MMIO_INTERRUPT_ACK => {
                self.interrupt_status.fetch_and(!val, Ordering::SeqCst);
            }
            VIRTIO_MMIO_STATUS => {
                if val == 0 {
                    // Device reset
                    self.device_status = 0;
                    self.queues = [
                        VirtioQueueState::default(),
                        VirtioQueueState::default(),
                        VirtioQueueState::default(),
                    ];
                    self.connections.clear();
                    self.rx_queue.clear();
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

impl MutDeviceMmio for VirtioVsock {
    fn mmio_read(&mut self, _base: MmioAddress, offset: MmioAddressOffset, data: &mut [u8]) {
        self.handle_mmio_read(offset, data);
    }

    fn mmio_write(&mut self, _base: MmioAddress, offset: MmioAddressOffset, data: &[u8]) {
        self.handle_mmio_write(offset, data);
    }
}
