//! File and directory handle management for virtio-fs.
//!
//! Tracks open files and directories with their associated state.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::os::unix::fs::OpenOptionsExt;
use std::path::Path;

use nix::libc;

use super::inode::errno_from_io;

pub const MAX_HANDLES: usize = 4096;

pub struct DirEntry {
    pub ino: u64,
    pub name: String,
    pub typ: u32,
}

pub enum HandleKind {
    File(File),
    Dir(Vec<DirEntry>),
}

pub struct Handle {
    pub kind: HandleKind,
    pub ino: u64,
    pub flags: u32,
}

pub struct HandleTable {
    handles: HashMap<u64, Handle>,
    next_fh: u64,
}

impl HandleTable {
    pub fn new() -> Self {
        Self {
            handles: HashMap::new(),
            next_fh: 1,
        }
    }

    pub fn open_file(
        &mut self,
        path: &Path,
        flags: u32,
        ino: u64,
        read_only: bool,
    ) -> Result<u64, i32> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(libc::EMFILE);
        }

        let linux_flags = flags as i32;

        let read = (linux_flags & libc::O_ACCMODE) == libc::O_RDONLY
            || (linux_flags & libc::O_ACCMODE) == libc::O_RDWR;
        let write = (linux_flags & libc::O_ACCMODE) == libc::O_WRONLY
            || (linux_flags & libc::O_ACCMODE) == libc::O_RDWR;

        if write && read_only {
            return Err(libc::EROFS);
        }

        let file = OpenOptions::new()
            .read(read)
            .write(write)
            .append((linux_flags & libc::O_APPEND) != 0)
            .truncate((linux_flags & libc::O_TRUNC) != 0)
            .custom_flags(linux_flags & !(libc::O_ACCMODE | libc::O_CREAT | libc::O_EXCL))
            .open(path)
            .map_err(|e| errno_from_io(&e))?;

        let fh = self.next_fh;
        self.next_fh += 1;

        self.handles.insert(
            fh,
            Handle {
                kind: HandleKind::File(file),
                ino,
                flags,
            },
        );

        Ok(fh)
    }

    pub fn create_file(
        &mut self,
        path: &Path,
        flags: u32,
        mode: u32,
        ino: u64,
    ) -> Result<u64, i32> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(libc::EMFILE);
        }

        let linux_flags = flags as i32;

        let read = (linux_flags & libc::O_ACCMODE) == libc::O_RDONLY
            || (linux_flags & libc::O_ACCMODE) == libc::O_RDWR;
        let write = (linux_flags & libc::O_ACCMODE) == libc::O_WRONLY
            || (linux_flags & libc::O_ACCMODE) == libc::O_RDWR
            || (linux_flags & libc::O_CREAT) != 0;

        let file = OpenOptions::new()
            .read(read)
            .write(write)
            .create(true)
            .truncate((linux_flags & libc::O_TRUNC) != 0)
            .mode(mode)
            .open(path)
            .map_err(|e| errno_from_io(&e))?;

        let fh = self.next_fh;
        self.next_fh += 1;

        self.handles.insert(
            fh,
            Handle {
                kind: HandleKind::File(file),
                ino,
                flags,
            },
        );

        Ok(fh)
    }

    pub fn open_dir(&mut self, path: &Path, ino: u64) -> Result<u64, i32> {
        if self.handles.len() >= MAX_HANDLES {
            return Err(libc::EMFILE);
        }

        let read_dir = std::fs::read_dir(path).map_err(|e| errno_from_io(&e))?;

        let mut entries = Vec::new();

        entries.push(DirEntry {
            ino,
            name: ".".to_string(),
            typ: libc::DT_DIR as u32,
        });

        entries.push(DirEntry {
            ino: 0,
            name: "..".to_string(),
            typ: libc::DT_DIR as u32,
        });

        for entry in read_dir {
            let entry = entry.map_err(|e| errno_from_io(&e))?;
            let file_type = entry.file_type().map_err(|e| errno_from_io(&e))?;

            let typ = if file_type.is_dir() {
                libc::DT_DIR
            } else if file_type.is_symlink() {
                libc::DT_LNK
            } else if file_type.is_file() {
                libc::DT_REG
            } else {
                libc::DT_UNKNOWN
            } as u32;

            let name = entry.file_name().to_string_lossy().to_string();

            entries.push(DirEntry { ino: 0, name, typ });
        }

        let fh = self.next_fh;
        self.next_fh += 1;

        self.handles.insert(
            fh,
            Handle {
                kind: HandleKind::Dir(entries),
                ino,
                flags: 0,
            },
        );

        Ok(fh)
    }

    pub fn get(&self, fh: u64) -> Option<&Handle> {
        self.handles.get(&fh)
    }

    pub fn get_mut(&mut self, fh: u64) -> Option<&mut Handle> {
        self.handles.get_mut(&fh)
    }

    pub fn release(&mut self, fh: u64) -> Option<Handle> {
        self.handles.remove(&fh)
    }

    pub fn read_file(&mut self, fh: u64, offset: u64, size: u32) -> Result<Vec<u8>, i32> {
        let handle = self.handles.get_mut(&fh).ok_or(libc::EBADF)?;

        let file = match &mut handle.kind {
            HandleKind::File(f) => f,
            HandleKind::Dir(_) => return Err(libc::EISDIR),
        };

        file.seek(SeekFrom::Start(offset))
            .map_err(|e| errno_from_io(&e))?;

        let mut buf = vec![0u8; size as usize];
        let n = file.read(&mut buf).map_err(|e| errno_from_io(&e))?;
        buf.truncate(n);

        Ok(buf)
    }

    pub fn write_file(&mut self, fh: u64, offset: u64, data: &[u8]) -> Result<u32, i32> {
        let handle = self.handles.get_mut(&fh).ok_or(libc::EBADF)?;

        let file = match &mut handle.kind {
            HandleKind::File(f) => f,
            HandleKind::Dir(_) => return Err(libc::EISDIR),
        };

        file.seek(SeekFrom::Start(offset))
            .map_err(|e| errno_from_io(&e))?;

        let n = file.write(data).map_err(|e| errno_from_io(&e))?;

        Ok(n as u32)
    }

    pub fn flush_file(&mut self, fh: u64) -> Result<(), i32> {
        let handle = self.handles.get_mut(&fh).ok_or(libc::EBADF)?;

        if let HandleKind::File(f) = &mut handle.kind {
            f.flush().map_err(|e| errno_from_io(&e))?;
        }

        Ok(())
    }

    pub fn fsync_file(&mut self, fh: u64, datasync: bool) -> Result<(), i32> {
        let handle = self.handles.get_mut(&fh).ok_or(libc::EBADF)?;

        if let HandleKind::File(f) = &mut handle.kind {
            if datasync {
                f.sync_data().map_err(|e| errno_from_io(&e))?;
            } else {
                f.sync_all().map_err(|e| errno_from_io(&e))?;
            }
        }

        Ok(())
    }

    pub fn read_dir(&self, fh: u64, offset: u64) -> Result<&[DirEntry], i32> {
        let handle = self.handles.get(&fh).ok_or(libc::EBADF)?;

        let entries = match &handle.kind {
            HandleKind::Dir(e) => e,
            HandleKind::File(_) => return Err(libc::ENOTDIR),
        };

        let offset = offset as usize;
        if offset >= entries.len() {
            return Ok(&[]);
        }

        Ok(&entries[offset..])
    }
}

impl Default for HandleTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn open_and_read_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "hello world").unwrap();

        let mut table = HandleTable::new();
        let fh = table
            .open_file(&path, libc::O_RDONLY as u32, 2, false)
            .unwrap();

        let data = table.read_file(fh, 0, 100).unwrap();
        assert_eq!(data, b"hello world");

        table.release(fh);
        assert!(table.get(fh).is_none());
    }

    #[test]
    fn open_and_write_file() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "").unwrap();

        let mut table = HandleTable::new();
        let fh = table
            .open_file(&path, libc::O_RDWR as u32, 2, false)
            .unwrap();

        let n = table.write_file(fh, 0, b"hello").unwrap();
        assert_eq!(n, 5);

        table.release(fh);

        let content = fs::read_to_string(&path).unwrap();
        assert_eq!(content, "hello");
    }

    #[test]
    fn read_only_prevents_write() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "hello").unwrap();

        let mut table = HandleTable::new();
        let result = table.open_file(&path, libc::O_RDWR as u32, 2, true);

        assert_eq!(result, Err(libc::EROFS));
    }

    #[test]
    fn open_and_read_dir() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("a.txt"), "a").unwrap();
        fs::write(tmp.path().join("b.txt"), "b").unwrap();

        let mut table = HandleTable::new();
        let fh = table.open_dir(tmp.path(), 1).unwrap();

        let entries = table.read_dir(fh, 0).unwrap();
        assert!(entries.len() >= 4);
        assert!(entries.iter().any(|e| e.name == "."));
        assert!(entries.iter().any(|e| e.name == ".."));
        assert!(entries.iter().any(|e| e.name == "a.txt"));
        assert!(entries.iter().any(|e| e.name == "b.txt"));
    }

    #[test]
    fn handle_limit_enforced() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("test.txt");
        fs::write(&path, "").unwrap();

        let mut table = HandleTable::new();

        for i in 0..MAX_HANDLES {
            let result = table.open_file(&path, libc::O_RDONLY as u32, i as u64 + 2, false);
            assert!(result.is_ok(), "failed at {}", i);
        }

        let result = table.open_file(&path, libc::O_RDONLY as u32, 99999, false);
        assert_eq!(result, Err(libc::EMFILE));
    }
}
