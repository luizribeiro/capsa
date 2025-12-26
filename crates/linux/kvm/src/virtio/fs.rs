//! Virtio-fs device implementation.
//!
//! Provides shared directory access between host and guest using the FUSE protocol
//! over virtio MMIO transport.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

use kvm_ioctls::VmFd;
use nix::libc;
use vm_device::MutDeviceMmio;
use vm_device::bus::{MmioAddress, MmioAddressOffset};
use vm_memory::{Bytes, GuestAddress, GuestMemoryMmap};

use super::{MAX_DESCRIPTOR_LEN, validate_queue_addresses};
use crate::fuse::{
    FATTR_ATIME, FATTR_ATIME_NOW, FATTR_GID, FATTR_MODE, FATTR_MTIME, FATTR_MTIME_NOW, FATTR_SIZE,
    FATTR_UID, FOPEN_KEEP_CACHE, FUSE_ASYNC_READ, FUSE_ATOMIC_O_TRUNC, FUSE_BIG_WRITES,
    FUSE_EXPORT_SUPPORT, FUSE_IN_HEADER_SIZE, FUSE_KERNEL_MINOR_VERSION, FUSE_KERNEL_VERSION,
    FUSE_MAX_PAGES, FUSE_PARALLEL_DIROPS, FuseAttrOut, FuseCreateIn, FuseDirent, FuseEntryOut,
    FuseFlushIn, FuseForgetIn, FuseFsyncIn, FuseInHeader, FuseInitIn, FuseInitOut, FuseLinkIn,
    FuseMkdirIn, FuseOpcode, FuseOpenIn, FuseOpenOut, FuseReadIn, FuseReleaseIn, FuseRenameIn,
    FuseSetattrIn, FuseStatfsOut, FuseWriteIn, FuseWriteOut, HandleTable, InodeTable,
    errno_from_io, error_response, extract_name, metadata_to_attr, success_response,
    success_response_empty,
};

const VIRTIO_ID_FS: u32 = 26;

const REQUEST_QUEUE_INDEX: usize = 1;
const QUEUE_SIZE: u16 = 256;
const NUM_QUEUES: usize = 2;

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

const VIRTIO_INT_USED_RING: u32 = 1;

const VIRTIO_F_VERSION_1: u64 = 1 << 32;

const FS_TAG_SIZE: usize = 36;
const FS_CONFIG_SIZE: usize = FS_TAG_SIZE + 4;

const MAX_READ_SIZE: u32 = 1024 * 1024;
const MAX_WRITE_SIZE: u32 = 1024 * 1024;

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

pub struct VirtioFs {
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

    memory: Option<Arc<GuestMemoryMmap>>,

    tag: String,
    #[allow(dead_code)]
    host_path: PathBuf,
    read_only: bool,

    inodes: InodeTable,
    handles: HandleTable,
    fuse_initialized: bool,
}

impl VirtioFs {
    pub fn new(
        host_path: PathBuf,
        tag: String,
        read_only: bool,
        vm_fd: Arc<VmFd>,
        irq: u32,
    ) -> Self {
        let device_features = VIRTIO_F_VERSION_1;

        Self {
            device_features,
            driver_features: 0,
            device_features_sel: 0,
            driver_features_sel: 0,
            device_status: 0,
            queue_sel: 0,
            queues: Default::default(),
            interrupt_status: AtomicU32::new(0),
            vm_fd,
            irq,
            memory: None,
            tag,
            host_path: host_path.clone(),
            read_only,
            inodes: InodeTable::new(host_path),
            handles: HandleTable::new(),
            fuse_initialized: false,
        }
    }

    pub fn set_memory(&mut self, memory: Arc<GuestMemoryMmap>) {
        self.memory = Some(memory);
    }

    fn inject_interrupt(&self) {
        self.interrupt_status
            .fetch_or(VIRTIO_INT_USED_RING, Ordering::SeqCst);
        let _ = self.vm_fd.set_irq_line(self.irq, true);
        let _ = self.vm_fd.set_irq_line(self.irq, false);
    }

    fn process_request_queue(&mut self) {
        let memory = match &self.memory {
            Some(m) => m.clone(),
            None => return,
        };

        let queue_state = &self.queues[REQUEST_QUEUE_INDEX];
        if !queue_state.ready {
            return;
        }

        // Copy queue state to avoid borrowing self
        let desc_table = queue_state.desc_table;
        let avail_ring = queue_state.avail_ring;
        let used_ring = queue_state.used_ring;
        let queue_size = queue_state.size;
        let mut next_avail = queue_state.next_avail;
        let mut next_used = queue_state.next_used;

        // Collect all requests first
        let mut requests_to_process: Vec<(u16, Vec<u8>)> = Vec::new();

        loop {
            let avail_idx: u16 = memory
                .read_obj(GuestAddress(avail_ring + 2))
                .unwrap_or(next_avail);
            if next_avail == avail_idx {
                break;
            }

            let desc_idx_addr = avail_ring + 4 + ((next_avail as u64 % queue_size as u64) * 2);
            let desc_idx: u16 = memory.read_obj(GuestAddress(desc_idx_addr)).unwrap_or(0);

            // Read the request data from descriptor chain
            let request_data =
                Self::read_descriptor_chain(&memory, desc_table, queue_size, desc_idx);
            requests_to_process.push((desc_idx, request_data));

            next_avail = next_avail.wrapping_add(1);
        }

        if requests_to_process.is_empty() {
            return;
        }

        // Process each request and collect responses
        let mut responses: Vec<(u16, Vec<u8>)> = Vec::new();
        for (desc_idx, request_data) in requests_to_process {
            let response = self.handle_fuse_request(&request_data);
            responses.push((desc_idx, response));
        }

        // Write responses back to guest
        for (desc_idx, response) in responses {
            Self::write_response_to_chain(&memory, desc_table, queue_size, desc_idx, &response);

            let response_len = response.len() as u32;
            let used_entry_addr = used_ring + 4 + ((next_used as u64 % queue_size as u64) * 8);
            memory
                .write_obj(desc_idx as u32, GuestAddress(used_entry_addr))
                .ok();
            memory
                .write_obj(response_len, GuestAddress(used_entry_addr + 4))
                .ok();

            next_used = next_used.wrapping_add(1);
            memory
                .write_obj(next_used, GuestAddress(used_ring + 2))
                .ok();
        }

        // Update queue state
        self.queues[REQUEST_QUEUE_INDEX].next_avail = next_avail;
        self.queues[REQUEST_QUEUE_INDEX].next_used = next_used;

        self.inject_interrupt();
    }

    fn read_descriptor_chain(
        memory: &GuestMemoryMmap,
        desc_table: u64,
        _queue_size: u16,
        first_desc_idx: u16,
    ) -> Vec<u8> {
        let mut request_data = Vec::new();
        let mut desc_idx = first_desc_idx;

        loop {
            let desc_addr = desc_table + (desc_idx as u64 * 16);
            let addr: u64 = memory.read_obj(GuestAddress(desc_addr)).unwrap_or(0);
            let len: u32 = memory.read_obj(GuestAddress(desc_addr + 8)).unwrap_or(0);
            let flags: u16 = memory.read_obj(GuestAddress(desc_addr + 12)).unwrap_or(0);
            let next: u16 = memory.read_obj(GuestAddress(desc_addr + 14)).unwrap_or(0);

            let len = len.min(MAX_DESCRIPTOR_LEN);

            // Read from device-readable descriptors (not write-only)
            if (flags & 2) == 0 {
                let mut buf = vec![0u8; len as usize];
                if memory.read_slice(&mut buf, GuestAddress(addr)).is_ok() {
                    request_data.extend_from_slice(&buf);
                }
            }

            // Check NEXT flag
            if (flags & 1) == 0 {
                break;
            }
            desc_idx = next;
        }

        request_data
    }

    fn write_response_to_chain(
        memory: &GuestMemoryMmap,
        desc_table: u64,
        _queue_size: u16,
        first_desc_idx: u16,
        response: &[u8],
    ) {
        let mut desc_idx = first_desc_idx;
        let mut response_offset = 0usize;

        loop {
            let desc_addr = desc_table + (desc_idx as u64 * 16);
            let addr: u64 = memory.read_obj(GuestAddress(desc_addr)).unwrap_or(0);
            let len: u32 = memory.read_obj(GuestAddress(desc_addr + 8)).unwrap_or(0);
            let flags: u16 = memory.read_obj(GuestAddress(desc_addr + 12)).unwrap_or(0);
            let next: u16 = memory.read_obj(GuestAddress(desc_addr + 14)).unwrap_or(0);

            // Write to device-writable descriptors
            if (flags & 2) != 0 && response_offset < response.len() {
                let to_write = (response.len() - response_offset).min(len as usize);
                let _ = memory.write_slice(
                    &response[response_offset..response_offset + to_write],
                    GuestAddress(addr),
                );
                response_offset += to_write;
            }

            // Check NEXT flag
            if (flags & 1) == 0 {
                break;
            }
            desc_idx = next;
        }
    }

    fn handle_fuse_request(&mut self, request: &[u8]) -> Vec<u8> {
        let header = match FuseInHeader::from_bytes(request) {
            Some(h) => h,
            None => return error_response(0, libc::EINVAL),
        };

        let body = &request[FUSE_IN_HEADER_SIZE..];

        let opcode = match FuseOpcode::try_from(header.opcode) {
            Ok(op) => op,
            Err(_) => return error_response(header.unique, libc::ENOSYS),
        };

        match opcode {
            FuseOpcode::Init => self.handle_init(header.unique, body),
            FuseOpcode::Destroy => self.handle_destroy(header.unique),
            FuseOpcode::Lookup => self.handle_lookup(header.unique, header.nodeid, body),
            FuseOpcode::Forget => self.handle_forget(header.nodeid, body),
            FuseOpcode::Getattr => self.handle_getattr(header.unique, header.nodeid, body),
            FuseOpcode::Setattr => self.handle_setattr(header.unique, header.nodeid, body),
            FuseOpcode::Readlink => self.handle_readlink(header.unique, header.nodeid),
            FuseOpcode::Symlink => self.handle_symlink(header.unique, header.nodeid, body),
            FuseOpcode::Mknod => self.handle_mknod(header.unique, header.nodeid, body),
            FuseOpcode::Mkdir => self.handle_mkdir(header.unique, header.nodeid, body),
            FuseOpcode::Unlink => self.handle_unlink(header.unique, header.nodeid, body),
            FuseOpcode::Rmdir => self.handle_rmdir(header.unique, header.nodeid, body),
            FuseOpcode::Rename => self.handle_rename(header.unique, header.nodeid, body),
            FuseOpcode::Link => self.handle_link(header.unique, header.nodeid, body),
            FuseOpcode::Open => self.handle_open(header.unique, header.nodeid, body),
            FuseOpcode::Read => self.handle_read(header.unique, body),
            FuseOpcode::Write => self.handle_write(header.unique, body),
            FuseOpcode::Statfs => self.handle_statfs(header.unique, header.nodeid),
            FuseOpcode::Release => self.handle_release(header.unique, body),
            FuseOpcode::Fsync => self.handle_fsync(header.unique, body),
            FuseOpcode::Opendir => self.handle_opendir(header.unique, header.nodeid),
            FuseOpcode::Readdir => self.handle_readdir(header.unique, body),
            FuseOpcode::Releasedir => self.handle_releasedir(header.unique, body),
            FuseOpcode::Fsyncdir => self.handle_fsyncdir(header.unique, body),
            FuseOpcode::Access => self.handle_access(header.unique, header.nodeid, body),
            FuseOpcode::Create => self.handle_create(header.unique, header.nodeid, body),
            FuseOpcode::Flush => self.handle_flush(header.unique, body),
            _ => {
                tracing::debug!("unimplemented FUSE opcode: {:?}", opcode);
                error_response(header.unique, libc::ENOSYS)
            }
        }
    }

    fn handle_init(&mut self, unique: u64, body: &[u8]) -> Vec<u8> {
        let init_in = match FuseInitIn::from_bytes(body) {
            Some(i) => i,
            None => return error_response(unique, libc::EINVAL),
        };

        if init_in.major < FUSE_KERNEL_VERSION {
            return error_response(unique, libc::EPROTO);
        }

        self.fuse_initialized = true;

        let out = FuseInitOut {
            major: FUSE_KERNEL_VERSION,
            minor: FUSE_KERNEL_MINOR_VERSION,
            max_readahead: init_in.max_readahead,
            flags: FUSE_ASYNC_READ
                | FUSE_BIG_WRITES
                | FUSE_ATOMIC_O_TRUNC
                | FUSE_EXPORT_SUPPORT
                | FUSE_PARALLEL_DIROPS
                | FUSE_MAX_PAGES,
            max_background: 0,
            congestion_threshold: 0,
            max_write: MAX_WRITE_SIZE,
            time_gran: 1,
            max_pages: (MAX_READ_SIZE / 4096) as u16,
            map_alignment: 0,
            unused: [0; 8],
        };

        success_response(unique, &out.to_bytes())
    }

    fn handle_destroy(&mut self, unique: u64) -> Vec<u8> {
        self.fuse_initialized = false;
        success_response_empty(unique)
    }

    fn handle_lookup(&mut self, unique: u64, parent: u64, body: &[u8]) -> Vec<u8> {
        let name = match extract_name(body) {
            Some(n) => n,
            None => return error_response(unique, libc::EINVAL),
        };

        let ino = match self.inodes.lookup(parent, name) {
            Ok(i) => i,
            Err(e) => return error_response(unique, e),
        };

        let path = match self.inodes.get_path(ino) {
            Some(p) => p.to_path_buf(),
            None => return error_response(unique, libc::ENOENT),
        };

        let metadata = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => return error_response(unique, errno_from_io(&e)),
        };

        let attr = metadata_to_attr(ino, &metadata);
        let out = FuseEntryOut {
            nodeid: ino,
            generation: 0,
            entry_valid: 1,
            attr_valid: 1,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr,
        };

        success_response(unique, &out.to_bytes())
    }

    fn handle_forget(&mut self, nodeid: u64, body: &[u8]) -> Vec<u8> {
        if let Some(forget) = FuseForgetIn::from_bytes(body) {
            self.inodes.forget(nodeid, forget.nlookup);
        }
        Vec::new()
    }

    fn handle_getattr(&mut self, unique: u64, nodeid: u64, _body: &[u8]) -> Vec<u8> {
        let path = match self.inodes.get_path(nodeid) {
            Some(p) => p.to_path_buf(),
            None => return error_response(unique, libc::ENOENT),
        };

        let metadata = match std::fs::metadata(&path) {
            Ok(m) => m,
            Err(e) => return error_response(unique, errno_from_io(&e)),
        };

        let attr = metadata_to_attr(nodeid, &metadata);
        let out = FuseAttrOut {
            attr_valid: 1,
            attr_valid_nsec: 0,
            dummy: 0,
            attr,
        };

        success_response(unique, &out.to_bytes())
    }

    fn handle_setattr(&mut self, unique: u64, nodeid: u64, body: &[u8]) -> Vec<u8> {
        if self.read_only {
            return error_response(unique, libc::EROFS);
        }

        let setattr = match FuseSetattrIn::from_bytes(body) {
            Some(s) => s,
            None => return error_response(unique, libc::EINVAL),
        };

        let path = match self.inodes.get_path(nodeid) {
            Some(p) => p.to_path_buf(),
            None => return error_response(unique, libc::ENOENT),
        };

        if (setattr.valid & FATTR_SIZE) != 0 {
            let file = match std::fs::OpenOptions::new().write(true).open(&path) {
                Ok(f) => f,
                Err(e) => return error_response(unique, errno_from_io(&e)),
            };
            if let Err(e) = file.set_len(setattr.size) {
                return error_response(unique, errno_from_io(&e));
            }
        }

        if (setattr.valid & FATTR_MODE) != 0 {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(setattr.mode);
            if let Err(e) = std::fs::set_permissions(&path, perms) {
                return error_response(unique, errno_from_io(&e));
            }
        }

        if (setattr.valid & (FATTR_UID | FATTR_GID)) != 0 {
            let uid = if (setattr.valid & FATTR_UID) != 0 {
                setattr.uid
            } else {
                u32::MAX
            };
            let gid = if (setattr.valid & FATTR_GID) != 0 {
                setattr.gid
            } else {
                u32::MAX
            };
            let path_cstr =
                std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default();
            let ret = unsafe { libc::chown(path_cstr.as_ptr(), uid, gid) };
            if ret != 0 {
                return error_response(unique, errno_from_io(&std::io::Error::last_os_error()));
            }
        }

        if (setattr.valid & (FATTR_ATIME | FATTR_MTIME)) != 0 {
            use nix::sys::stat::{UtimensatFlags, utimensat};
            use nix::sys::time::TimeSpec;

            let atime = if (setattr.valid & FATTR_ATIME_NOW) != 0 {
                TimeSpec::new(0, libc::UTIME_NOW)
            } else if (setattr.valid & FATTR_ATIME) != 0 {
                TimeSpec::new(setattr.atime as i64, setattr.atimensec as i64)
            } else {
                TimeSpec::new(0, libc::UTIME_OMIT)
            };

            let mtime = if (setattr.valid & FATTR_MTIME_NOW) != 0 {
                TimeSpec::new(0, libc::UTIME_NOW)
            } else if (setattr.valid & FATTR_MTIME) != 0 {
                TimeSpec::new(setattr.mtime as i64, setattr.mtimensec as i64)
            } else {
                TimeSpec::new(0, libc::UTIME_OMIT)
            };

            if let Err(e) = utimensat(None, &path, &atime, &mtime, UtimensatFlags::NoFollowSymlink)
            {
                return error_response(unique, e as i32);
            }
        }

        self.handle_getattr(unique, nodeid, &[])
    }

    fn handle_readlink(&mut self, unique: u64, nodeid: u64) -> Vec<u8> {
        let path = match self.inodes.get_path(nodeid) {
            Some(p) => p.to_path_buf(),
            None => return error_response(unique, libc::ENOENT),
        };

        let target = match std::fs::read_link(&path) {
            Ok(t) => t,
            Err(e) => return error_response(unique, errno_from_io(&e)),
        };

        success_response(unique, target.to_string_lossy().as_bytes())
    }

    fn handle_symlink(&mut self, unique: u64, parent: u64, body: &[u8]) -> Vec<u8> {
        if self.read_only {
            return error_response(unique, libc::EROFS);
        }

        let name = match extract_name(body) {
            Some(n) => n,
            None => return error_response(unique, libc::EINVAL),
        };

        let name_end = body.iter().position(|&b| b == 0).unwrap_or(body.len());
        let target = match extract_name(&body[name_end + 1..]) {
            Some(t) => t,
            None => return error_response(unique, libc::EINVAL),
        };

        let new_path = match self.inodes.validate_parent_and_name(parent, name) {
            Ok(p) => p,
            Err(e) => return error_response(unique, e),
        };

        if let Err(e) = std::os::unix::fs::symlink(target, &new_path) {
            return error_response(unique, errno_from_io(&e));
        }

        let ino = match self.inodes.lookup_path(&new_path) {
            Ok(i) => i,
            Err(e) => return error_response(unique, e),
        };

        let metadata = match std::fs::symlink_metadata(&new_path) {
            Ok(m) => m,
            Err(e) => return error_response(unique, errno_from_io(&e)),
        };

        let attr = metadata_to_attr(ino, &metadata);
        let out = FuseEntryOut {
            nodeid: ino,
            generation: 0,
            entry_valid: 1,
            attr_valid: 1,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr,
        };

        success_response(unique, &out.to_bytes())
    }

    fn handle_mknod(&mut self, unique: u64, _parent: u64, _body: &[u8]) -> Vec<u8> {
        error_response(unique, libc::ENOSYS)
    }

    fn handle_mkdir(&mut self, unique: u64, parent: u64, body: &[u8]) -> Vec<u8> {
        if self.read_only {
            return error_response(unique, libc::EROFS);
        }

        let mkdir_in = match FuseMkdirIn::from_bytes(body) {
            Some(m) => m,
            None => return error_response(unique, libc::EINVAL),
        };

        let name = match extract_name(&body[8..]) {
            Some(n) => n,
            None => return error_response(unique, libc::EINVAL),
        };

        let new_path = match self.inodes.validate_parent_and_name(parent, name) {
            Ok(p) => p,
            Err(e) => return error_response(unique, e),
        };

        use std::os::unix::fs::DirBuilderExt;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(mkdir_in.mode & !mkdir_in.umask);

        if let Err(e) = builder.create(&new_path) {
            return error_response(unique, errno_from_io(&e));
        }

        let ino = match self.inodes.lookup_path(&new_path) {
            Ok(i) => i,
            Err(e) => return error_response(unique, e),
        };

        let metadata = match std::fs::metadata(&new_path) {
            Ok(m) => m,
            Err(e) => return error_response(unique, errno_from_io(&e)),
        };

        let attr = metadata_to_attr(ino, &metadata);
        let out = FuseEntryOut {
            nodeid: ino,
            generation: 0,
            entry_valid: 1,
            attr_valid: 1,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr,
        };

        success_response(unique, &out.to_bytes())
    }

    fn handle_unlink(&mut self, unique: u64, parent: u64, body: &[u8]) -> Vec<u8> {
        if self.read_only {
            return error_response(unique, libc::EROFS);
        }

        let name = match extract_name(body) {
            Some(n) => n,
            None => return error_response(unique, libc::EINVAL),
        };

        let path = match self.inodes.validate_parent_and_name(parent, name) {
            Ok(p) => p,
            Err(e) => return error_response(unique, e),
        };

        if let Err(e) = std::fs::remove_file(&path) {
            return error_response(unique, errno_from_io(&e));
        }

        self.inodes.remove_by_path(&path);

        success_response_empty(unique)
    }

    fn handle_rmdir(&mut self, unique: u64, parent: u64, body: &[u8]) -> Vec<u8> {
        if self.read_only {
            return error_response(unique, libc::EROFS);
        }

        let name = match extract_name(body) {
            Some(n) => n,
            None => return error_response(unique, libc::EINVAL),
        };

        let path = match self.inodes.validate_parent_and_name(parent, name) {
            Ok(p) => p,
            Err(e) => return error_response(unique, e),
        };

        if let Err(e) = std::fs::remove_dir(&path) {
            return error_response(unique, errno_from_io(&e));
        }

        self.inodes.remove_by_path(&path);

        success_response_empty(unique)
    }

    fn handle_rename(&mut self, unique: u64, parent: u64, body: &[u8]) -> Vec<u8> {
        if self.read_only {
            return error_response(unique, libc::EROFS);
        }

        let rename_in = match FuseRenameIn::from_bytes(body) {
            Some(r) => r,
            None => return error_response(unique, libc::EINVAL),
        };

        let names = &body[8..];
        let old_name = match extract_name(names) {
            Some(n) => n,
            None => return error_response(unique, libc::EINVAL),
        };

        let old_name_end = names.iter().position(|&b| b == 0).unwrap_or(names.len());
        let new_name = match extract_name(&names[old_name_end + 1..]) {
            Some(n) => n,
            None => return error_response(unique, libc::EINVAL),
        };

        let old_path = match self.inodes.validate_parent_and_name(parent, old_name) {
            Ok(p) => p,
            Err(e) => return error_response(unique, e),
        };

        let new_path = match self
            .inodes
            .validate_parent_and_name(rename_in.newdir, new_name)
        {
            Ok(p) => p,
            Err(e) => return error_response(unique, e),
        };

        if let Err(e) = std::fs::rename(&old_path, &new_path) {
            return error_response(unique, errno_from_io(&e));
        }

        self.inodes.remove_by_path(&old_path);

        success_response_empty(unique)
    }

    fn handle_link(&mut self, unique: u64, parent: u64, body: &[u8]) -> Vec<u8> {
        if self.read_only {
            return error_response(unique, libc::EROFS);
        }

        let link_in = match FuseLinkIn::from_bytes(body) {
            Some(l) => l,
            None => return error_response(unique, libc::EINVAL),
        };

        let name = match extract_name(&body[8..]) {
            Some(n) => n,
            None => return error_response(unique, libc::EINVAL),
        };

        let old_path = match self.inodes.get_path(link_in.oldnodeid) {
            Some(p) => p.to_path_buf(),
            None => return error_response(unique, libc::ENOENT),
        };

        let new_path = match self.inodes.validate_parent_and_name(parent, name) {
            Ok(p) => p,
            Err(e) => return error_response(unique, e),
        };

        if let Err(e) = std::fs::hard_link(&old_path, &new_path) {
            return error_response(unique, errno_from_io(&e));
        }

        let ino = match self.inodes.lookup_path(&new_path) {
            Ok(i) => i,
            Err(e) => return error_response(unique, e),
        };

        let metadata = match std::fs::metadata(&new_path) {
            Ok(m) => m,
            Err(e) => return error_response(unique, errno_from_io(&e)),
        };

        let attr = metadata_to_attr(ino, &metadata);
        let out = FuseEntryOut {
            nodeid: ino,
            generation: 0,
            entry_valid: 1,
            attr_valid: 1,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr,
        };

        success_response(unique, &out.to_bytes())
    }

    fn handle_open(&mut self, unique: u64, nodeid: u64, body: &[u8]) -> Vec<u8> {
        let open_in = match FuseOpenIn::from_bytes(body) {
            Some(o) => o,
            None => return error_response(unique, libc::EINVAL),
        };

        let path = match self.inodes.get_path(nodeid) {
            Some(p) => p.to_path_buf(),
            None => return error_response(unique, libc::ENOENT),
        };

        let fh = match self
            .handles
            .open_file(&path, open_in.flags, nodeid, self.read_only)
        {
            Ok(f) => f,
            Err(e) => return error_response(unique, e),
        };

        let out = FuseOpenOut {
            fh,
            open_flags: FOPEN_KEEP_CACHE,
            padding: 0,
        };

        success_response(unique, &out.to_bytes())
    }

    fn handle_read(&mut self, unique: u64, body: &[u8]) -> Vec<u8> {
        let read_in = match FuseReadIn::from_bytes(body) {
            Some(r) => r,
            None => return error_response(unique, libc::EINVAL),
        };

        let size = read_in.size.min(MAX_READ_SIZE);

        let data = match self.handles.read_file(read_in.fh, read_in.offset, size) {
            Ok(d) => d,
            Err(e) => return error_response(unique, e),
        };

        success_response(unique, &data)
    }

    fn handle_write(&mut self, unique: u64, body: &[u8]) -> Vec<u8> {
        if self.read_only {
            return error_response(unique, libc::EROFS);
        }

        let write_in = match FuseWriteIn::from_bytes(body) {
            Some(w) => w,
            None => return error_response(unique, libc::EINVAL),
        };

        let data_offset = std::mem::size_of::<FuseWriteIn>();
        if body.len() < data_offset {
            return error_response(unique, libc::EINVAL);
        }

        let data = &body[data_offset..];
        let size = (write_in.size as usize)
            .min(data.len())
            .min(MAX_WRITE_SIZE as usize);

        let n = match self
            .handles
            .write_file(write_in.fh, write_in.offset, &data[..size])
        {
            Ok(n) => n,
            Err(e) => return error_response(unique, e),
        };

        let out = FuseWriteOut {
            size: n,
            padding: 0,
        };
        success_response(unique, &out.to_bytes())
    }

    fn handle_statfs(&mut self, unique: u64, nodeid: u64) -> Vec<u8> {
        let path = match self.inodes.get_path(nodeid) {
            Some(p) => p.to_path_buf(),
            None => return error_response(unique, libc::ENOENT),
        };

        let mut statfs: libc::statfs = unsafe { std::mem::zeroed() };
        let path_cstr =
            std::ffi::CString::new(path.to_string_lossy().as_bytes()).unwrap_or_default();

        let ret = unsafe { libc::statfs(path_cstr.as_ptr(), &mut statfs) };
        if ret != 0 {
            return error_response(unique, errno_from_io(&std::io::Error::last_os_error()));
        }

        let out = FuseStatfsOut {
            blocks: statfs.f_blocks,
            bfree: statfs.f_bfree,
            bavail: statfs.f_bavail,
            files: statfs.f_files,
            ffree: statfs.f_ffree,
            bsize: statfs.f_bsize as u32,
            namelen: statfs.f_namelen as u32,
            frsize: statfs.f_frsize as u32,
            padding: 0,
            spare: [0; 6],
        };

        success_response(unique, &out.to_bytes())
    }

    fn handle_release(&mut self, unique: u64, body: &[u8]) -> Vec<u8> {
        let release_in = match FuseReleaseIn::from_bytes(body) {
            Some(r) => r,
            None => return error_response(unique, libc::EINVAL),
        };

        self.handles.release(release_in.fh);
        success_response_empty(unique)
    }

    fn handle_fsync(&mut self, unique: u64, body: &[u8]) -> Vec<u8> {
        let fsync_in = match FuseFsyncIn::from_bytes(body) {
            Some(f) => f,
            None => return error_response(unique, libc::EINVAL),
        };

        let datasync = (fsync_in.fsync_flags & 1) != 0;

        if let Err(e) = self.handles.fsync_file(fsync_in.fh, datasync) {
            return error_response(unique, e);
        }

        success_response_empty(unique)
    }

    fn handle_opendir(&mut self, unique: u64, nodeid: u64) -> Vec<u8> {
        let path = match self.inodes.get_path(nodeid) {
            Some(p) => p.to_path_buf(),
            None => return error_response(unique, libc::ENOENT),
        };

        let fh = match self.handles.open_dir(&path, nodeid) {
            Ok(f) => f,
            Err(e) => return error_response(unique, e),
        };

        let out = FuseOpenOut {
            fh,
            open_flags: 0,
            padding: 0,
        };

        success_response(unique, &out.to_bytes())
    }

    fn handle_readdir(&mut self, unique: u64, body: &[u8]) -> Vec<u8> {
        let read_in = match FuseReadIn::from_bytes(body) {
            Some(r) => r,
            None => return error_response(unique, libc::EINVAL),
        };

        let entries = match self.handles.read_dir(read_in.fh, read_in.offset) {
            Ok(e) => e,
            Err(e) => return error_response(unique, e),
        };

        let mut buf = Vec::new();
        let max_size = read_in.size as usize;

        for (i, entry) in entries.iter().enumerate() {
            let name_bytes = entry.name.as_bytes();
            let entry_size = FuseDirent::entry_size(name_bytes.len());

            if buf.len() + entry_size > max_size {
                break;
            }

            let dirent = FuseDirent {
                ino: entry.ino,
                off: (read_in.offset as usize + i + 1) as u64,
                namelen: name_bytes.len() as u32,
                typ: entry.typ,
            };

            buf.extend_from_slice(&dirent.to_bytes());
            buf.extend_from_slice(name_bytes);

            let padding = entry_size - FuseDirent::entry_size(0) - name_bytes.len();
            buf.extend(std::iter::repeat_n(0u8, padding));
        }

        success_response(unique, &buf)
    }

    fn handle_releasedir(&mut self, unique: u64, body: &[u8]) -> Vec<u8> {
        let release_in = match FuseReleaseIn::from_bytes(body) {
            Some(r) => r,
            None => return error_response(unique, libc::EINVAL),
        };

        self.handles.release(release_in.fh);
        success_response_empty(unique)
    }

    fn handle_fsyncdir(&mut self, unique: u64, _body: &[u8]) -> Vec<u8> {
        success_response_empty(unique)
    }

    fn handle_access(&mut self, unique: u64, nodeid: u64, _body: &[u8]) -> Vec<u8> {
        let path = match self.inodes.get_path(nodeid) {
            Some(p) => p.to_path_buf(),
            None => return error_response(unique, libc::ENOENT),
        };

        match std::fs::metadata(&path) {
            Ok(_) => success_response_empty(unique),
            Err(e) => error_response(unique, errno_from_io(&e)),
        }
    }

    fn handle_create(&mut self, unique: u64, parent: u64, body: &[u8]) -> Vec<u8> {
        if self.read_only {
            return error_response(unique, libc::EROFS);
        }

        let create_in = match FuseCreateIn::from_bytes(body) {
            Some(c) => c,
            None => return error_response(unique, libc::EINVAL),
        };

        let name = match extract_name(&body[16..]) {
            Some(n) => n,
            None => return error_response(unique, libc::EINVAL),
        };

        let new_path = match self.inodes.validate_parent_and_name(parent, name) {
            Ok(p) => p,
            Err(e) => return error_response(unique, e),
        };

        let mode = create_in.mode & !create_in.umask;

        let ino = match self.inodes.lookup_path(&new_path) {
            Ok(i) => i,
            Err(_) => {
                let _ = std::fs::File::create(&new_path);
                match self.inodes.lookup_path(&new_path) {
                    Ok(i) => i,
                    Err(e) => return error_response(unique, e),
                }
            }
        };

        let fh = match self
            .handles
            .create_file(&new_path, create_in.flags, mode, ino)
        {
            Ok(f) => f,
            Err(e) => return error_response(unique, e),
        };

        let metadata = match std::fs::metadata(&new_path) {
            Ok(m) => m,
            Err(e) => return error_response(unique, errno_from_io(&e)),
        };

        let attr = metadata_to_attr(ino, &metadata);
        let entry = FuseEntryOut {
            nodeid: ino,
            generation: 0,
            entry_valid: 1,
            attr_valid: 1,
            entry_valid_nsec: 0,
            attr_valid_nsec: 0,
            attr,
        };

        let open = FuseOpenOut {
            fh,
            open_flags: FOPEN_KEEP_CACHE,
            padding: 0,
        };

        let mut buf = entry.to_bytes();
        buf.extend_from_slice(&open.to_bytes());
        success_response(unique, &buf)
    }

    fn handle_flush(&mut self, unique: u64, body: &[u8]) -> Vec<u8> {
        let flush_in = match FuseFlushIn::from_bytes(body) {
            Some(f) => f,
            None => return error_response(unique, libc::EINVAL),
        };

        if let Err(e) = self.handles.flush_file(flush_in.fh) {
            return error_response(unique, e);
        }

        success_response_empty(unique)
    }

    fn handle_mmio_read(&self, offset: u64, data: &mut [u8]) {
        // Config space may be read with different sizes (1, 2, 4 bytes)
        if offset >= VIRTIO_MMIO_CONFIG && offset < VIRTIO_MMIO_CONFIG + FS_CONFIG_SIZE as u64 {
            let config_offset = (offset - VIRTIO_MMIO_CONFIG) as usize;
            for (i, byte) in data.iter_mut().enumerate() {
                let idx = config_offset + i;
                *byte = if idx < FS_TAG_SIZE {
                    let tag_bytes = self.tag.as_bytes();
                    if idx < tag_bytes.len() {
                        tag_bytes[idx]
                    } else {
                        0
                    }
                } else if idx < FS_TAG_SIZE + 4 {
                    // num_request_queues = 1 (little-endian)
                    let num_queues_bytes = 1u32.to_le_bytes();
                    num_queues_bytes[idx - FS_TAG_SIZE]
                } else {
                    0
                };
            }
            return;
        }

        let val: u32 = match offset {
            VIRTIO_MMIO_MAGIC => VIRTIO_MMIO_MAGIC_VALUE,
            VIRTIO_MMIO_VERSION => 2,
            VIRTIO_MMIO_DEVICE_ID => VIRTIO_ID_FS,
            VIRTIO_MMIO_VENDOR_ID => 0x554d4551,
            VIRTIO_MMIO_DEVICE_FEATURES => {
                if self.device_features_sel == 0 {
                    self.device_features as u32
                } else {
                    (self.device_features >> 32) as u32
                }
            }
            VIRTIO_MMIO_QUEUE_NUM_MAX => QUEUE_SIZE as u32,
            VIRTIO_MMIO_QUEUE_READY => {
                if (self.queue_sel as usize) < NUM_QUEUES {
                    self.queues[self.queue_sel as usize].ready as u32
                } else {
                    0
                }
            }
            VIRTIO_MMIO_INTERRUPT_STATUS => self.interrupt_status.load(Ordering::SeqCst),
            VIRTIO_MMIO_STATUS => self.device_status,
            _ => 0,
        };

        let val_bytes = val.to_le_bytes();
        for (i, byte) in data.iter_mut().enumerate() {
            if i < 4 {
                *byte = val_bytes[i];
            }
        }
    }

    fn handle_mmio_write(&mut self, offset: u64, data: &[u8]) {
        if data.len() < 4 {
            return;
        }
        let val = u32::from_le_bytes(data[..4].try_into().unwrap_or_default());

        match offset {
            VIRTIO_MMIO_DEVICE_FEATURES_SEL => {
                self.device_features_sel = val;
            }
            VIRTIO_MMIO_DRIVER_FEATURES_SEL => {
                self.driver_features_sel = val;
            }
            VIRTIO_MMIO_DRIVER_FEATURES => {
                if self.driver_features_sel == 0 {
                    self.driver_features =
                        (self.driver_features & 0xFFFF_FFFF_0000_0000) | (val as u64);
                } else {
                    self.driver_features =
                        (self.driver_features & 0x0000_0000_FFFF_FFFF) | ((val as u64) << 32);
                }
            }
            VIRTIO_MMIO_QUEUE_SEL => {
                self.queue_sel = val;
            }
            VIRTIO_MMIO_QUEUE_NUM => {
                if (self.queue_sel as usize) < NUM_QUEUES {
                    self.queues[self.queue_sel as usize].size = val as u16;
                }
            }
            VIRTIO_MMIO_QUEUE_READY => {
                if (self.queue_sel as usize) < NUM_QUEUES {
                    let queue = &mut self.queues[self.queue_sel as usize];
                    if val == 1 {
                        if let Some(memory) = &self.memory {
                            if validate_queue_addresses(
                                memory,
                                queue.desc_table,
                                queue.avail_ring,
                                queue.used_ring,
                                queue.size,
                            ) {
                                queue.ready = true;
                            } else {
                                tracing::warn!(
                                    "virtio-fs: invalid queue addresses, not setting ready"
                                );
                            }
                        }
                    } else {
                        queue.ready = false;
                    }
                }
            }
            VIRTIO_MMIO_QUEUE_NOTIFY => {
                if val == REQUEST_QUEUE_INDEX as u32 {
                    self.process_request_queue();
                }
            }
            VIRTIO_MMIO_INTERRUPT_ACK => {
                self.interrupt_status.fetch_and(!val, Ordering::SeqCst);
            }
            VIRTIO_MMIO_STATUS => {
                self.device_status = val;
                if val == 0 {
                    self.queues = Default::default();
                    self.driver_features = 0;
                    self.fuse_initialized = false;
                }
            }
            VIRTIO_MMIO_QUEUE_DESC_LOW => {
                if (self.queue_sel as usize) < NUM_QUEUES {
                    let queue = &mut self.queues[self.queue_sel as usize];
                    queue.desc_table = (queue.desc_table & 0xFFFF_FFFF_0000_0000) | (val as u64);
                }
            }
            VIRTIO_MMIO_QUEUE_DESC_HIGH => {
                if (self.queue_sel as usize) < NUM_QUEUES {
                    let queue = &mut self.queues[self.queue_sel as usize];
                    queue.desc_table =
                        (queue.desc_table & 0x0000_0000_FFFF_FFFF) | ((val as u64) << 32);
                }
            }
            VIRTIO_MMIO_QUEUE_AVAIL_LOW => {
                if (self.queue_sel as usize) < NUM_QUEUES {
                    let queue = &mut self.queues[self.queue_sel as usize];
                    queue.avail_ring = (queue.avail_ring & 0xFFFF_FFFF_0000_0000) | (val as u64);
                }
            }
            VIRTIO_MMIO_QUEUE_AVAIL_HIGH => {
                if (self.queue_sel as usize) < NUM_QUEUES {
                    let queue = &mut self.queues[self.queue_sel as usize];
                    queue.avail_ring =
                        (queue.avail_ring & 0x0000_0000_FFFF_FFFF) | ((val as u64) << 32);
                }
            }
            VIRTIO_MMIO_QUEUE_USED_LOW => {
                if (self.queue_sel as usize) < NUM_QUEUES {
                    let queue = &mut self.queues[self.queue_sel as usize];
                    queue.used_ring = (queue.used_ring & 0xFFFF_FFFF_0000_0000) | (val as u64);
                }
            }
            VIRTIO_MMIO_QUEUE_USED_HIGH => {
                if (self.queue_sel as usize) < NUM_QUEUES {
                    let queue = &mut self.queues[self.queue_sel as usize];
                    queue.used_ring =
                        (queue.used_ring & 0x0000_0000_FFFF_FFFF) | ((val as u64) << 32);
                }
            }
            _ => {}
        }
    }
}

impl MutDeviceMmio for VirtioFs {
    fn mmio_read(&mut self, _base: MmioAddress, offset: MmioAddressOffset, data: &mut [u8]) {
        self.handle_mmio_read(offset, data);
    }

    fn mmio_write(&mut self, _base: MmioAddress, offset: MmioAddressOffset, data: &[u8]) {
        self.handle_mmio_write(offset, data);
    }
}
