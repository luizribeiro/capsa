//! FUSE protocol types and parsing.
//!
//! Implements the FUSE kernel protocol for virtio-fs.
//! Based on Linux include/uapi/linux/fuse.h

#![allow(dead_code)]

use std::ffi::CStr;

pub const FUSE_KERNEL_VERSION: u32 = 7;
pub const FUSE_KERNEL_MINOR_VERSION: u32 = 31;

#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FuseOpcode {
    Lookup = 1,
    Forget = 2,
    Getattr = 3,
    Setattr = 4,
    Readlink = 5,
    Symlink = 6,
    Mknod = 8,
    Mkdir = 9,
    Unlink = 10,
    Rmdir = 11,
    Rename = 12,
    Link = 13,
    Open = 14,
    Read = 15,
    Write = 16,
    Statfs = 17,
    Release = 18,
    Fsync = 20,
    Setxattr = 21,
    Getxattr = 22,
    Listxattr = 23,
    Removexattr = 24,
    Flush = 25,
    Init = 26,
    Opendir = 27,
    Readdir = 28,
    Releasedir = 29,
    Fsyncdir = 30,
    Getlk = 31,
    Setlk = 32,
    Setlkw = 33,
    Access = 34,
    Create = 35,
    Interrupt = 36,
    Bmap = 37,
    Destroy = 38,
    Ioctl = 39,
    Poll = 40,
    NotifyReply = 41,
    BatchForget = 42,
    Fallocate = 43,
    Readdirplus = 44,
    Rename2 = 45,
    Lseek = 46,
    CopyFileRange = 47,
    SetupMapping = 48,
    RemoveMapping = 49,
}

impl TryFrom<u32> for FuseOpcode {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        match value {
            1 => Ok(Self::Lookup),
            2 => Ok(Self::Forget),
            3 => Ok(Self::Getattr),
            4 => Ok(Self::Setattr),
            5 => Ok(Self::Readlink),
            6 => Ok(Self::Symlink),
            8 => Ok(Self::Mknod),
            9 => Ok(Self::Mkdir),
            10 => Ok(Self::Unlink),
            11 => Ok(Self::Rmdir),
            12 => Ok(Self::Rename),
            13 => Ok(Self::Link),
            14 => Ok(Self::Open),
            15 => Ok(Self::Read),
            16 => Ok(Self::Write),
            17 => Ok(Self::Statfs),
            18 => Ok(Self::Release),
            20 => Ok(Self::Fsync),
            21 => Ok(Self::Setxattr),
            22 => Ok(Self::Getxattr),
            23 => Ok(Self::Listxattr),
            24 => Ok(Self::Removexattr),
            25 => Ok(Self::Flush),
            26 => Ok(Self::Init),
            27 => Ok(Self::Opendir),
            28 => Ok(Self::Readdir),
            29 => Ok(Self::Releasedir),
            30 => Ok(Self::Fsyncdir),
            31 => Ok(Self::Getlk),
            32 => Ok(Self::Setlk),
            33 => Ok(Self::Setlkw),
            34 => Ok(Self::Access),
            35 => Ok(Self::Create),
            36 => Ok(Self::Interrupt),
            37 => Ok(Self::Bmap),
            38 => Ok(Self::Destroy),
            39 => Ok(Self::Ioctl),
            40 => Ok(Self::Poll),
            41 => Ok(Self::NotifyReply),
            42 => Ok(Self::BatchForget),
            43 => Ok(Self::Fallocate),
            44 => Ok(Self::Readdirplus),
            45 => Ok(Self::Rename2),
            46 => Ok(Self::Lseek),
            47 => Ok(Self::CopyFileRange),
            48 => Ok(Self::SetupMapping),
            49 => Ok(Self::RemoveMapping),
            _ => Err(()),
        }
    }
}

/// FUSE request header (40 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseInHeader {
    pub len: u32,
    pub opcode: u32,
    pub unique: u64,
    pub nodeid: u64,
    pub uid: u32,
    pub gid: u32,
    pub pid: u32,
    pub padding: u32,
}

pub const FUSE_IN_HEADER_SIZE: usize = std::mem::size_of::<FuseInHeader>();

impl FuseInHeader {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_IN_HEADER_SIZE {
            return None;
        }
        Some(Self {
            len: u32::from_le_bytes(data[0..4].try_into().ok()?),
            opcode: u32::from_le_bytes(data[4..8].try_into().ok()?),
            unique: u64::from_le_bytes(data[8..16].try_into().ok()?),
            nodeid: u64::from_le_bytes(data[16..24].try_into().ok()?),
            uid: u32::from_le_bytes(data[24..28].try_into().ok()?),
            gid: u32::from_le_bytes(data[28..32].try_into().ok()?),
            pid: u32::from_le_bytes(data[32..36].try_into().ok()?),
            padding: u32::from_le_bytes(data[36..40].try_into().ok()?),
        })
    }
}

/// FUSE response header (16 bytes).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseOutHeader {
    pub len: u32,
    pub error: i32,
    pub unique: u64,
}

pub const FUSE_OUT_HEADER_SIZE: usize = std::mem::size_of::<FuseOutHeader>();

impl FuseOutHeader {
    pub fn to_bytes(self) -> [u8; FUSE_OUT_HEADER_SIZE] {
        let mut buf = [0u8; FUSE_OUT_HEADER_SIZE];
        buf[0..4].copy_from_slice(&self.len.to_le_bytes());
        buf[4..8].copy_from_slice(&self.error.to_le_bytes());
        buf[8..16].copy_from_slice(&self.unique.to_le_bytes());
        buf
    }
}

/// File attributes.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseAttr {
    pub ino: u64,
    pub size: u64,
    pub blocks: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub atimensec: u32,
    pub mtimensec: u32,
    pub ctimensec: u32,
    pub mode: u32,
    pub nlink: u32,
    pub uid: u32,
    pub gid: u32,
    pub rdev: u32,
    pub blksize: u32,
    pub padding: u32,
}

pub const FUSE_ATTR_SIZE: usize = std::mem::size_of::<FuseAttr>();

impl FuseAttr {
    pub fn to_bytes(self) -> [u8; FUSE_ATTR_SIZE] {
        let mut buf = [0u8; FUSE_ATTR_SIZE];
        buf[0..8].copy_from_slice(&self.ino.to_le_bytes());
        buf[8..16].copy_from_slice(&self.size.to_le_bytes());
        buf[16..24].copy_from_slice(&self.blocks.to_le_bytes());
        buf[24..32].copy_from_slice(&self.atime.to_le_bytes());
        buf[32..40].copy_from_slice(&self.mtime.to_le_bytes());
        buf[40..48].copy_from_slice(&self.ctime.to_le_bytes());
        buf[48..52].copy_from_slice(&self.atimensec.to_le_bytes());
        buf[52..56].copy_from_slice(&self.mtimensec.to_le_bytes());
        buf[56..60].copy_from_slice(&self.ctimensec.to_le_bytes());
        buf[60..64].copy_from_slice(&self.mode.to_le_bytes());
        buf[64..68].copy_from_slice(&self.nlink.to_le_bytes());
        buf[68..72].copy_from_slice(&self.uid.to_le_bytes());
        buf[72..76].copy_from_slice(&self.gid.to_le_bytes());
        buf[76..80].copy_from_slice(&self.rdev.to_le_bytes());
        buf[80..84].copy_from_slice(&self.blksize.to_le_bytes());
        buf[84..88].copy_from_slice(&self.padding.to_le_bytes());
        buf
    }
}

/// Entry response (lookup, create, etc).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseEntryOut {
    pub nodeid: u64,
    pub generation: u64,
    pub entry_valid: u64,
    pub attr_valid: u64,
    pub entry_valid_nsec: u32,
    pub attr_valid_nsec: u32,
    pub attr: FuseAttr,
}

pub const FUSE_ENTRY_OUT_SIZE: usize = std::mem::size_of::<FuseEntryOut>();

impl FuseEntryOut {
    pub fn to_bytes(self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(FUSE_ENTRY_OUT_SIZE);
        buf.extend_from_slice(&self.nodeid.to_le_bytes());
        buf.extend_from_slice(&self.generation.to_le_bytes());
        buf.extend_from_slice(&self.entry_valid.to_le_bytes());
        buf.extend_from_slice(&self.attr_valid.to_le_bytes());
        buf.extend_from_slice(&self.entry_valid_nsec.to_le_bytes());
        buf.extend_from_slice(&self.attr_valid_nsec.to_le_bytes());
        buf.extend_from_slice(&self.attr.to_bytes());
        buf
    }
}

/// Getattr input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseGetattrIn {
    pub getattr_flags: u32,
    pub dummy: u32,
    pub fh: u64,
}

pub const FUSE_GETATTR_IN_SIZE: usize = std::mem::size_of::<FuseGetattrIn>();

impl FuseGetattrIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_GETATTR_IN_SIZE {
            return None;
        }
        Some(Self {
            getattr_flags: u32::from_le_bytes(data[0..4].try_into().ok()?),
            dummy: u32::from_le_bytes(data[4..8].try_into().ok()?),
            fh: u64::from_le_bytes(data[8..16].try_into().ok()?),
        })
    }
}

/// Getattr output.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseAttrOut {
    pub attr_valid: u64,
    pub attr_valid_nsec: u32,
    pub dummy: u32,
    pub attr: FuseAttr,
}

pub const FUSE_ATTR_OUT_SIZE: usize = std::mem::size_of::<FuseAttrOut>();

impl FuseAttrOut {
    pub fn to_bytes(self) -> Vec<u8> {
        let mut buf = Vec::with_capacity(FUSE_ATTR_OUT_SIZE);
        buf.extend_from_slice(&self.attr_valid.to_le_bytes());
        buf.extend_from_slice(&self.attr_valid_nsec.to_le_bytes());
        buf.extend_from_slice(&self.dummy.to_le_bytes());
        buf.extend_from_slice(&self.attr.to_bytes());
        buf
    }
}

/// Setattr input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseSetattrIn {
    pub valid: u32,
    pub padding: u32,
    pub fh: u64,
    pub size: u64,
    pub lock_owner: u64,
    pub atime: u64,
    pub mtime: u64,
    pub ctime: u64,
    pub atimensec: u32,
    pub mtimensec: u32,
    pub ctimensec: u32,
    pub mode: u32,
    pub unused4: u32,
    pub uid: u32,
    pub gid: u32,
    pub unused5: u32,
}

pub const FUSE_SETATTR_IN_SIZE: usize = std::mem::size_of::<FuseSetattrIn>();

impl FuseSetattrIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_SETATTR_IN_SIZE {
            return None;
        }
        Some(Self {
            valid: u32::from_le_bytes(data[0..4].try_into().ok()?),
            padding: u32::from_le_bytes(data[4..8].try_into().ok()?),
            fh: u64::from_le_bytes(data[8..16].try_into().ok()?),
            size: u64::from_le_bytes(data[16..24].try_into().ok()?),
            lock_owner: u64::from_le_bytes(data[24..32].try_into().ok()?),
            atime: u64::from_le_bytes(data[32..40].try_into().ok()?),
            mtime: u64::from_le_bytes(data[40..48].try_into().ok()?),
            ctime: u64::from_le_bytes(data[48..56].try_into().ok()?),
            atimensec: u32::from_le_bytes(data[56..60].try_into().ok()?),
            mtimensec: u32::from_le_bytes(data[60..64].try_into().ok()?),
            ctimensec: u32::from_le_bytes(data[64..68].try_into().ok()?),
            mode: u32::from_le_bytes(data[68..72].try_into().ok()?),
            unused4: u32::from_le_bytes(data[72..76].try_into().ok()?),
            uid: u32::from_le_bytes(data[76..80].try_into().ok()?),
            gid: u32::from_le_bytes(data[80..84].try_into().ok()?),
            unused5: u32::from_le_bytes(data[84..88].try_into().ok()?),
        })
    }
}

// Setattr valid flags
pub const FATTR_MODE: u32 = 1 << 0;
pub const FATTR_UID: u32 = 1 << 1;
pub const FATTR_GID: u32 = 1 << 2;
pub const FATTR_SIZE: u32 = 1 << 3;
pub const FATTR_ATIME: u32 = 1 << 4;
pub const FATTR_MTIME: u32 = 1 << 5;
pub const FATTR_FH: u32 = 1 << 6;
pub const FATTR_ATIME_NOW: u32 = 1 << 7;
pub const FATTR_MTIME_NOW: u32 = 1 << 8;
pub const FATTR_LOCKOWNER: u32 = 1 << 9;
pub const FATTR_CTIME: u32 = 1 << 10;

/// Init input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseInitIn {
    pub major: u32,
    pub minor: u32,
    pub max_readahead: u32,
    pub flags: u32,
}

pub const FUSE_INIT_IN_SIZE: usize = std::mem::size_of::<FuseInitIn>();

impl FuseInitIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_INIT_IN_SIZE {
            return None;
        }
        Some(Self {
            major: u32::from_le_bytes(data[0..4].try_into().ok()?),
            minor: u32::from_le_bytes(data[4..8].try_into().ok()?),
            max_readahead: u32::from_le_bytes(data[8..12].try_into().ok()?),
            flags: u32::from_le_bytes(data[12..16].try_into().ok()?),
        })
    }
}

/// Init output.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseInitOut {
    pub major: u32,
    pub minor: u32,
    pub max_readahead: u32,
    pub flags: u32,
    pub max_background: u16,
    pub congestion_threshold: u16,
    pub max_write: u32,
    pub time_gran: u32,
    pub max_pages: u16,
    pub map_alignment: u16,
    pub unused: [u32; 8],
}

pub const FUSE_INIT_OUT_SIZE: usize = std::mem::size_of::<FuseInitOut>();

impl FuseInitOut {
    pub fn to_bytes(self) -> [u8; FUSE_INIT_OUT_SIZE] {
        let mut buf = [0u8; FUSE_INIT_OUT_SIZE];
        buf[0..4].copy_from_slice(&self.major.to_le_bytes());
        buf[4..8].copy_from_slice(&self.minor.to_le_bytes());
        buf[8..12].copy_from_slice(&self.max_readahead.to_le_bytes());
        buf[12..16].copy_from_slice(&self.flags.to_le_bytes());
        buf[16..18].copy_from_slice(&self.max_background.to_le_bytes());
        buf[18..20].copy_from_slice(&self.congestion_threshold.to_le_bytes());
        buf[20..24].copy_from_slice(&self.max_write.to_le_bytes());
        buf[24..28].copy_from_slice(&self.time_gran.to_le_bytes());
        buf[28..30].copy_from_slice(&self.max_pages.to_le_bytes());
        buf[30..32].copy_from_slice(&self.map_alignment.to_le_bytes());
        // unused[8] stays zero
        buf
    }
}

// FUSE init flags
pub const FUSE_ASYNC_READ: u32 = 1 << 0;
pub const FUSE_POSIX_LOCKS: u32 = 1 << 1;
pub const FUSE_FILE_OPS: u32 = 1 << 2;
pub const FUSE_ATOMIC_O_TRUNC: u32 = 1 << 3;
pub const FUSE_EXPORT_SUPPORT: u32 = 1 << 4;
pub const FUSE_BIG_WRITES: u32 = 1 << 5;
pub const FUSE_DONT_MASK: u32 = 1 << 6;
pub const FUSE_SPLICE_WRITE: u32 = 1 << 7;
pub const FUSE_SPLICE_MOVE: u32 = 1 << 8;
pub const FUSE_SPLICE_READ: u32 = 1 << 9;
pub const FUSE_FLOCK_LOCKS: u32 = 1 << 10;
pub const FUSE_HAS_IOCTL_DIR: u32 = 1 << 11;
pub const FUSE_AUTO_INVAL_DATA: u32 = 1 << 12;
pub const FUSE_DO_READDIRPLUS: u32 = 1 << 13;
pub const FUSE_READDIRPLUS_AUTO: u32 = 1 << 14;
pub const FUSE_ASYNC_DIO: u32 = 1 << 15;
pub const FUSE_WRITEBACK_CACHE: u32 = 1 << 16;
pub const FUSE_NO_OPEN_SUPPORT: u32 = 1 << 17;
pub const FUSE_PARALLEL_DIROPS: u32 = 1 << 18;
pub const FUSE_HANDLE_KILLPRIV: u32 = 1 << 19;
pub const FUSE_POSIX_ACL: u32 = 1 << 20;
pub const FUSE_ABORT_ERROR: u32 = 1 << 21;
pub const FUSE_MAX_PAGES: u32 = 1 << 22;
pub const FUSE_CACHE_SYMLINKS: u32 = 1 << 23;
pub const FUSE_NO_OPENDIR_SUPPORT: u32 = 1 << 24;
pub const FUSE_EXPLICIT_INVAL_DATA: u32 = 1 << 25;

/// Forget input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseForgetIn {
    pub nlookup: u64,
}

pub const FUSE_FORGET_IN_SIZE: usize = std::mem::size_of::<FuseForgetIn>();

impl FuseForgetIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_FORGET_IN_SIZE {
            return None;
        }
        Some(Self {
            nlookup: u64::from_le_bytes(data[0..8].try_into().ok()?),
        })
    }
}

/// Open input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseOpenIn {
    pub flags: u32,
    pub unused: u32,
}

pub const FUSE_OPEN_IN_SIZE: usize = std::mem::size_of::<FuseOpenIn>();

impl FuseOpenIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_OPEN_IN_SIZE {
            return None;
        }
        Some(Self {
            flags: u32::from_le_bytes(data[0..4].try_into().ok()?),
            unused: u32::from_le_bytes(data[4..8].try_into().ok()?),
        })
    }
}

/// Open output.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseOpenOut {
    pub fh: u64,
    pub open_flags: u32,
    pub padding: u32,
}

pub const FUSE_OPEN_OUT_SIZE: usize = std::mem::size_of::<FuseOpenOut>();

impl FuseOpenOut {
    pub fn to_bytes(self) -> [u8; FUSE_OPEN_OUT_SIZE] {
        let mut buf = [0u8; FUSE_OPEN_OUT_SIZE];
        buf[0..8].copy_from_slice(&self.fh.to_le_bytes());
        buf[8..12].copy_from_slice(&self.open_flags.to_le_bytes());
        buf[12..16].copy_from_slice(&self.padding.to_le_bytes());
        buf
    }
}

// Open flags
pub const FOPEN_DIRECT_IO: u32 = 1 << 0;
pub const FOPEN_KEEP_CACHE: u32 = 1 << 1;
pub const FOPEN_NONSEEKABLE: u32 = 1 << 2;
pub const FOPEN_CACHE_DIR: u32 = 1 << 3;
pub const FOPEN_STREAM: u32 = 1 << 4;

/// Read input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseReadIn {
    pub fh: u64,
    pub offset: u64,
    pub size: u32,
    pub read_flags: u32,
    pub lock_owner: u64,
    pub flags: u32,
    pub padding: u32,
}

pub const FUSE_READ_IN_SIZE: usize = std::mem::size_of::<FuseReadIn>();

impl FuseReadIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_READ_IN_SIZE {
            return None;
        }
        Some(Self {
            fh: u64::from_le_bytes(data[0..8].try_into().ok()?),
            offset: u64::from_le_bytes(data[8..16].try_into().ok()?),
            size: u32::from_le_bytes(data[16..20].try_into().ok()?),
            read_flags: u32::from_le_bytes(data[20..24].try_into().ok()?),
            lock_owner: u64::from_le_bytes(data[24..32].try_into().ok()?),
            flags: u32::from_le_bytes(data[32..36].try_into().ok()?),
            padding: u32::from_le_bytes(data[36..40].try_into().ok()?),
        })
    }
}

/// Write input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseWriteIn {
    pub fh: u64,
    pub offset: u64,
    pub size: u32,
    pub write_flags: u32,
    pub lock_owner: u64,
    pub flags: u32,
    pub padding: u32,
}

pub const FUSE_WRITE_IN_SIZE: usize = std::mem::size_of::<FuseWriteIn>();

impl FuseWriteIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_WRITE_IN_SIZE {
            return None;
        }
        Some(Self {
            fh: u64::from_le_bytes(data[0..8].try_into().ok()?),
            offset: u64::from_le_bytes(data[8..16].try_into().ok()?),
            size: u32::from_le_bytes(data[16..20].try_into().ok()?),
            write_flags: u32::from_le_bytes(data[20..24].try_into().ok()?),
            lock_owner: u64::from_le_bytes(data[24..32].try_into().ok()?),
            flags: u32::from_le_bytes(data[32..36].try_into().ok()?),
            padding: u32::from_le_bytes(data[36..40].try_into().ok()?),
        })
    }
}

/// Write output.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseWriteOut {
    pub size: u32,
    pub padding: u32,
}

pub const FUSE_WRITE_OUT_SIZE: usize = std::mem::size_of::<FuseWriteOut>();

impl FuseWriteOut {
    pub fn to_bytes(self) -> [u8; FUSE_WRITE_OUT_SIZE] {
        let mut buf = [0u8; FUSE_WRITE_OUT_SIZE];
        buf[0..4].copy_from_slice(&self.size.to_le_bytes());
        buf[4..8].copy_from_slice(&self.padding.to_le_bytes());
        buf
    }
}

/// Release input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseReleaseIn {
    pub fh: u64,
    pub flags: u32,
    pub release_flags: u32,
    pub lock_owner: u64,
}

pub const FUSE_RELEASE_IN_SIZE: usize = std::mem::size_of::<FuseReleaseIn>();

impl FuseReleaseIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_RELEASE_IN_SIZE {
            return None;
        }
        Some(Self {
            fh: u64::from_le_bytes(data[0..8].try_into().ok()?),
            flags: u32::from_le_bytes(data[8..12].try_into().ok()?),
            release_flags: u32::from_le_bytes(data[12..16].try_into().ok()?),
            lock_owner: u64::from_le_bytes(data[16..24].try_into().ok()?),
        })
    }
}

/// Flush input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseFlushIn {
    pub fh: u64,
    pub unused: u32,
    pub padding: u32,
    pub lock_owner: u64,
}

pub const FUSE_FLUSH_IN_SIZE: usize = std::mem::size_of::<FuseFlushIn>();

impl FuseFlushIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_FLUSH_IN_SIZE {
            return None;
        }
        Some(Self {
            fh: u64::from_le_bytes(data[0..8].try_into().ok()?),
            unused: u32::from_le_bytes(data[8..12].try_into().ok()?),
            padding: u32::from_le_bytes(data[12..16].try_into().ok()?),
            lock_owner: u64::from_le_bytes(data[16..24].try_into().ok()?),
        })
    }
}

/// Fsync input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseFsyncIn {
    pub fh: u64,
    pub fsync_flags: u32,
    pub padding: u32,
}

pub const FUSE_FSYNC_IN_SIZE: usize = std::mem::size_of::<FuseFsyncIn>();

impl FuseFsyncIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_FSYNC_IN_SIZE {
            return None;
        }
        Some(Self {
            fh: u64::from_le_bytes(data[0..8].try_into().ok()?),
            fsync_flags: u32::from_le_bytes(data[8..12].try_into().ok()?),
            padding: u32::from_le_bytes(data[12..16].try_into().ok()?),
        })
    }
}

/// Mkdir input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseMkdirIn {
    pub mode: u32,
    pub umask: u32,
}

pub const FUSE_MKDIR_IN_SIZE: usize = std::mem::size_of::<FuseMkdirIn>();

impl FuseMkdirIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_MKDIR_IN_SIZE {
            return None;
        }
        Some(Self {
            mode: u32::from_le_bytes(data[0..4].try_into().ok()?),
            umask: u32::from_le_bytes(data[4..8].try_into().ok()?),
        })
    }
}

/// Create input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseCreateIn {
    pub flags: u32,
    pub mode: u32,
    pub umask: u32,
    pub padding: u32,
}

pub const FUSE_CREATE_IN_SIZE: usize = std::mem::size_of::<FuseCreateIn>();

impl FuseCreateIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_CREATE_IN_SIZE {
            return None;
        }
        Some(Self {
            flags: u32::from_le_bytes(data[0..4].try_into().ok()?),
            mode: u32::from_le_bytes(data[4..8].try_into().ok()?),
            umask: u32::from_le_bytes(data[8..12].try_into().ok()?),
            padding: u32::from_le_bytes(data[12..16].try_into().ok()?),
        })
    }
}

/// Rename input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseRenameIn {
    pub newdir: u64,
}

pub const FUSE_RENAME_IN_SIZE: usize = std::mem::size_of::<FuseRenameIn>();

impl FuseRenameIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_RENAME_IN_SIZE {
            return None;
        }
        Some(Self {
            newdir: u64::from_le_bytes(data[0..8].try_into().ok()?),
        })
    }
}

/// Link input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseLinkIn {
    pub oldnodeid: u64,
}

pub const FUSE_LINK_IN_SIZE: usize = std::mem::size_of::<FuseLinkIn>();

impl FuseLinkIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_LINK_IN_SIZE {
            return None;
        }
        Some(Self {
            oldnodeid: u64::from_le_bytes(data[0..8].try_into().ok()?),
        })
    }
}

/// Mknod input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseMknodIn {
    pub mode: u32,
    pub rdev: u32,
    pub umask: u32,
    pub padding: u32,
}

pub const FUSE_MKNOD_IN_SIZE: usize = std::mem::size_of::<FuseMknodIn>();

impl FuseMknodIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_MKNOD_IN_SIZE {
            return None;
        }
        Some(Self {
            mode: u32::from_le_bytes(data[0..4].try_into().ok()?),
            rdev: u32::from_le_bytes(data[4..8].try_into().ok()?),
            umask: u32::from_le_bytes(data[8..12].try_into().ok()?),
            padding: u32::from_le_bytes(data[12..16].try_into().ok()?),
        })
    }
}

/// Access input.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseAccessIn {
    pub mask: u32,
    pub padding: u32,
}

pub const FUSE_ACCESS_IN_SIZE: usize = std::mem::size_of::<FuseAccessIn>();

impl FuseAccessIn {
    pub fn from_bytes(data: &[u8]) -> Option<Self> {
        if data.len() < FUSE_ACCESS_IN_SIZE {
            return None;
        }
        Some(Self {
            mask: u32::from_le_bytes(data[0..4].try_into().ok()?),
            padding: u32::from_le_bytes(data[4..8].try_into().ok()?),
        })
    }
}

/// Statfs output.
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseStatfsOut {
    pub blocks: u64,
    pub bfree: u64,
    pub bavail: u64,
    pub files: u64,
    pub ffree: u64,
    pub bsize: u32,
    pub namelen: u32,
    pub frsize: u32,
    pub padding: u32,
    pub spare: [u32; 6],
}

pub const FUSE_STATFS_OUT_SIZE: usize = std::mem::size_of::<FuseStatfsOut>();

impl FuseStatfsOut {
    pub fn to_bytes(self) -> [u8; FUSE_STATFS_OUT_SIZE] {
        let mut buf = [0u8; FUSE_STATFS_OUT_SIZE];
        buf[0..8].copy_from_slice(&self.blocks.to_le_bytes());
        buf[8..16].copy_from_slice(&self.bfree.to_le_bytes());
        buf[16..24].copy_from_slice(&self.bavail.to_le_bytes());
        buf[24..32].copy_from_slice(&self.files.to_le_bytes());
        buf[32..40].copy_from_slice(&self.ffree.to_le_bytes());
        buf[40..44].copy_from_slice(&self.bsize.to_le_bytes());
        buf[44..48].copy_from_slice(&self.namelen.to_le_bytes());
        buf[48..52].copy_from_slice(&self.frsize.to_le_bytes());
        buf[52..56].copy_from_slice(&self.padding.to_le_bytes());
        // spare[6] stays zero
        buf
    }
}

/// Directory entry (for readdir).
#[repr(C)]
#[derive(Clone, Copy, Default, Debug)]
pub struct FuseDirent {
    pub ino: u64,
    pub off: u64,
    pub namelen: u32,
    pub typ: u32,
    // name follows, padded to 8-byte boundary
}

pub const FUSE_DIRENT_SIZE: usize = std::mem::size_of::<FuseDirent>();

impl FuseDirent {
    pub fn to_bytes(self) -> [u8; FUSE_DIRENT_SIZE] {
        let mut buf = [0u8; FUSE_DIRENT_SIZE];
        buf[0..8].copy_from_slice(&self.ino.to_le_bytes());
        buf[8..16].copy_from_slice(&self.off.to_le_bytes());
        buf[16..20].copy_from_slice(&self.namelen.to_le_bytes());
        buf[20..24].copy_from_slice(&self.typ.to_le_bytes());
        buf
    }

    pub fn entry_size(name_len: usize) -> usize {
        (FUSE_DIRENT_SIZE + name_len + 7) & !7
    }
}

/// Extract a null-terminated string from a byte slice.
pub fn extract_name(data: &[u8]) -> Option<&str> {
    CStr::from_bytes_until_nul(data)
        .ok()
        .and_then(|s| s.to_str().ok())
}

/// Build an error response.
pub fn error_response(unique: u64, errno: i32) -> Vec<u8> {
    let header = FuseOutHeader {
        len: FUSE_OUT_HEADER_SIZE as u32,
        error: -errno,
        unique,
    };
    header.to_bytes().to_vec()
}

/// Build a success response with payload.
pub fn success_response(unique: u64, payload: &[u8]) -> Vec<u8> {
    let header = FuseOutHeader {
        len: (FUSE_OUT_HEADER_SIZE + payload.len()) as u32,
        error: 0,
        unique,
    };
    let mut buf = header.to_bytes().to_vec();
    buf.extend_from_slice(payload);
    buf
}

/// Build a success response with no payload.
pub fn success_response_empty(unique: u64) -> Vec<u8> {
    let header = FuseOutHeader {
        len: FUSE_OUT_HEADER_SIZE as u32,
        error: 0,
        unique,
    };
    header.to_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;
    use nix::libc;

    #[test]
    fn fuse_in_header_size() {
        assert_eq!(FUSE_IN_HEADER_SIZE, 40);
    }

    #[test]
    fn fuse_out_header_size() {
        assert_eq!(FUSE_OUT_HEADER_SIZE, 16);
    }

    #[test]
    fn fuse_attr_size() {
        assert_eq!(FUSE_ATTR_SIZE, 88);
    }

    #[test]
    fn parse_fuse_in_header() {
        let mut data = [0u8; 40];
        data[0..4].copy_from_slice(&100u32.to_le_bytes()); // len
        data[4..8].copy_from_slice(&26u32.to_le_bytes()); // opcode (INIT)
        data[8..16].copy_from_slice(&12345u64.to_le_bytes()); // unique
        data[16..24].copy_from_slice(&1u64.to_le_bytes()); // nodeid

        let header = FuseInHeader::from_bytes(&data).unwrap();
        assert_eq!(header.len, 100);
        assert_eq!(header.opcode, 26);
        assert_eq!(header.unique, 12345);
        assert_eq!(header.nodeid, 1);
    }

    #[test]
    fn serialize_fuse_out_header() {
        let header = FuseOutHeader {
            len: 16,
            error: 0,
            unique: 12345,
        };
        let bytes = header.to_bytes();
        assert_eq!(u32::from_le_bytes(bytes[0..4].try_into().unwrap()), 16);
        assert_eq!(i32::from_le_bytes(bytes[4..8].try_into().unwrap()), 0);
        assert_eq!(u64::from_le_bytes(bytes[8..16].try_into().unwrap()), 12345);
    }

    #[test]
    fn error_response_format() {
        let resp = error_response(999, libc::ENOENT);
        assert_eq!(resp.len(), 16);
        let header = FuseOutHeader {
            len: u32::from_le_bytes(resp[0..4].try_into().unwrap()),
            error: i32::from_le_bytes(resp[4..8].try_into().unwrap()),
            unique: u64::from_le_bytes(resp[8..16].try_into().unwrap()),
        };
        assert_eq!(header.len, 16);
        assert_eq!(header.error, -libc::ENOENT);
        assert_eq!(header.unique, 999);
    }

    #[test]
    fn dirent_entry_size_alignment() {
        assert_eq!(FuseDirent::entry_size(1), 32); // 24 + 1 -> 32
        assert_eq!(FuseDirent::entry_size(8), 32); // 24 + 8 -> 32
        assert_eq!(FuseDirent::entry_size(9), 40); // 24 + 9 -> 40
    }
}
