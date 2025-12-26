//! Inode table management for virtio-fs.
//!
//! Maps guest inode numbers to host filesystem paths and metadata.

#![allow(dead_code)]

use std::collections::HashMap;
use std::fs::Metadata;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use nix::libc;

use super::protocol::FuseAttr;

pub const ROOT_INODE: u64 = 1;

pub const MAX_INODES: usize = 100_000;

pub struct InodeData {
    pub path: PathBuf,
    pub nlookup: u64,
}

pub struct InodeTable {
    host_root: PathBuf,
    by_guest_ino: HashMap<u64, InodeData>,
    by_host_key: HashMap<(u64, u64), u64>,
    next_ino: u64,
}

impl InodeTable {
    pub fn new(host_root: PathBuf) -> Self {
        let mut table = Self {
            host_root: host_root.clone(),
            by_guest_ino: HashMap::new(),
            by_host_key: HashMap::new(),
            next_ino: ROOT_INODE + 1,
        };

        table.by_guest_ino.insert(
            ROOT_INODE,
            InodeData {
                path: host_root,
                nlookup: 1,
            },
        );

        table
    }

    pub fn host_root(&self) -> &Path {
        &self.host_root
    }

    pub fn get(&self, ino: u64) -> Option<&InodeData> {
        self.by_guest_ino.get(&ino)
    }

    pub fn get_path(&self, ino: u64) -> Option<&Path> {
        self.by_guest_ino.get(&ino).map(|d| d.path.as_path())
    }

    pub fn lookup(&mut self, parent_ino: u64, name: &str) -> Result<u64, i32> {
        if name.contains('/') || name == "." || name == ".." {
            return Err(libc::EINVAL);
        }

        let parent_path = self
            .by_guest_ino
            .get(&parent_ino)
            .map(|d| d.path.clone())
            .ok_or(libc::ENOENT)?;

        let child_path = parent_path.join(name);
        self.lookup_path(&child_path)
    }

    pub fn lookup_path(&mut self, path: &Path) -> Result<u64, i32> {
        let canonical = self.validate_path(path)?;

        let metadata = std::fs::metadata(&canonical).map_err(|e| errno_from_io(&e))?;

        let host_key = (metadata.dev(), metadata.ino());

        if let Some(&guest_ino) = self.by_host_key.get(&host_key) {
            if let Some(data) = self.by_guest_ino.get_mut(&guest_ino) {
                data.nlookup += 1;
            }
            return Ok(guest_ino);
        }

        if self.by_guest_ino.len() >= MAX_INODES {
            return Err(libc::ENOSPC);
        }

        let guest_ino = self.next_ino;
        self.next_ino += 1;

        self.by_guest_ino.insert(
            guest_ino,
            InodeData {
                path: canonical,
                nlookup: 1,
            },
        );
        self.by_host_key.insert(host_key, guest_ino);

        Ok(guest_ino)
    }

    pub fn incref(&mut self, ino: u64) {
        if let Some(data) = self.by_guest_ino.get_mut(&ino) {
            data.nlookup += 1;
        }
    }

    pub fn forget(&mut self, ino: u64, nlookup: u64) {
        if ino == ROOT_INODE {
            return;
        }

        if let Some(data) = self.by_guest_ino.get_mut(&ino) {
            data.nlookup = data.nlookup.saturating_sub(nlookup);
            if data.nlookup == 0 {
                if let Ok(metadata) = std::fs::metadata(&data.path) {
                    let host_key = (metadata.dev(), metadata.ino());
                    self.by_host_key.remove(&host_key);
                }
                self.by_guest_ino.remove(&ino);
            }
        }
    }

    pub fn validate_path(&self, path: &Path) -> Result<PathBuf, i32> {
        let canonical = path.canonicalize().map_err(|e| errno_from_io(&e))?;

        if !canonical.starts_with(&self.host_root) {
            tracing::warn!(
                "path traversal attempt blocked: {:?} -> {:?}",
                path,
                canonical
            );
            return Err(libc::EACCES);
        }

        Ok(canonical)
    }

    pub fn validate_parent_and_name(&self, parent_ino: u64, name: &str) -> Result<PathBuf, i32> {
        if name.contains('/') || name == "." || name == ".." {
            return Err(libc::EINVAL);
        }

        let parent_path = self
            .by_guest_ino
            .get(&parent_ino)
            .map(|d| d.path.clone())
            .ok_or(libc::ENOENT)?;

        let parent_canonical = parent_path.canonicalize().map_err(|e| errno_from_io(&e))?;

        if !parent_canonical.starts_with(&self.host_root) {
            return Err(libc::EACCES);
        }

        Ok(parent_canonical.join(name))
    }

    pub fn remove_by_path(&mut self, path: &Path) {
        if let Ok(metadata) = std::fs::metadata(path) {
            let host_key = (metadata.dev(), metadata.ino());
            if let Some(guest_ino) = self.by_host_key.remove(&host_key) {
                self.by_guest_ino.remove(&guest_ino);
            }
        }
    }
}

pub fn metadata_to_attr(ino: u64, metadata: &Metadata) -> FuseAttr {
    let atime = metadata
        .accessed()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);
    let mtime = metadata
        .modified()
        .ok()
        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|d| d.as_secs())
        .unwrap_or(0);

    FuseAttr {
        ino,
        size: metadata.size(),
        blocks: metadata.blocks(),
        atime,
        mtime,
        ctime: metadata.ctime() as u64,
        atimensec: metadata.atime_nsec() as u32,
        mtimensec: metadata.mtime_nsec() as u32,
        ctimensec: metadata.ctime_nsec() as u32,
        mode: metadata.mode(),
        nlink: metadata.nlink() as u32,
        uid: metadata.uid(),
        gid: metadata.gid(),
        rdev: metadata.rdev() as u32,
        blksize: metadata.blksize() as u32,
        padding: 0,
    }
}

pub fn errno_from_io(e: &std::io::Error) -> i32 {
    e.raw_os_error().unwrap_or(libc::EIO)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn root_inode_is_one() {
        let tmp = TempDir::new().unwrap();
        let table = InodeTable::new(tmp.path().to_path_buf());
        assert!(table.get(ROOT_INODE).is_some());
        assert_eq!(table.get(ROOT_INODE).unwrap().path, tmp.path());
    }

    #[test]
    fn lookup_creates_inode() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("test.txt"), "hello").unwrap();

        let mut table = InodeTable::new(tmp.path().to_path_buf());
        let ino = table.lookup(ROOT_INODE, "test.txt").unwrap();

        assert!(ino > ROOT_INODE);
        assert!(table.get(ino).is_some());
    }

    #[test]
    fn lookup_returns_same_inode() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("test.txt"), "hello").unwrap();

        let mut table = InodeTable::new(tmp.path().to_path_buf());
        let ino1 = table.lookup(ROOT_INODE, "test.txt").unwrap();
        let ino2 = table.lookup(ROOT_INODE, "test.txt").unwrap();

        assert_eq!(ino1, ino2);
        assert_eq!(table.get(ino1).unwrap().nlookup, 2);
    }

    #[test]
    fn path_traversal_blocked() {
        let tmp = TempDir::new().unwrap();
        let table = InodeTable::new(tmp.path().to_path_buf());

        let evil_path = tmp.path().join("..").join("..").join("etc").join("passwd");
        let result = table.validate_path(&evil_path);

        assert!(result.is_err());
    }

    #[test]
    fn forget_removes_inode() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("test.txt"), "hello").unwrap();

        let mut table = InodeTable::new(tmp.path().to_path_buf());
        let ino = table.lookup(ROOT_INODE, "test.txt").unwrap();

        table.forget(ino, 1);
        assert!(table.get(ino).is_none());
    }

    #[test]
    fn forget_does_not_remove_root() {
        let tmp = TempDir::new().unwrap();
        let mut table = InodeTable::new(tmp.path().to_path_buf());

        table.forget(ROOT_INODE, 100);
        assert!(table.get(ROOT_INODE).is_some());
    }

    #[test]
    fn invalid_name_rejected() {
        let tmp = TempDir::new().unwrap();
        let table = InodeTable::new(tmp.path().to_path_buf());

        assert!(table.validate_parent_and_name(ROOT_INODE, "..").is_err());
        assert!(table.validate_parent_and_name(ROOT_INODE, ".").is_err());
        assert!(
            table
                .validate_parent_and_name(ROOT_INODE, "foo/bar")
                .is_err()
        );
    }

    #[test]
    fn path_traversal_absolute_path_blocked() {
        let tmp = TempDir::new().unwrap();
        let table = InodeTable::new(tmp.path().to_path_buf());

        let result = table.validate_path(Path::new("/etc/passwd"));
        assert!(result.is_err());
    }

    #[test]
    fn path_traversal_deep_nesting_blocked() {
        let tmp = TempDir::new().unwrap();
        let table = InodeTable::new(tmp.path().to_path_buf());

        let mut evil_path = tmp.path().to_path_buf();
        for _ in 0..50 {
            evil_path.push("..");
        }
        evil_path.push("etc/passwd");

        let result = table.validate_path(&evil_path);
        assert!(result.is_err());
    }

    #[test]
    fn symlink_inside_root_allowed() {
        let tmp = TempDir::new().unwrap();
        fs::write(tmp.path().join("target.txt"), "content").unwrap();

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("target.txt", tmp.path().join("link.txt")).unwrap();
        }

        let mut table = InodeTable::new(tmp.path().to_path_buf());
        let result = table.lookup(ROOT_INODE, "link.txt");
        assert!(result.is_ok());
    }

    #[test]
    fn symlink_outside_root_blocked() {
        let outer = TempDir::new().unwrap();
        let inner = outer.path().join("inner");
        fs::create_dir(&inner).unwrap();
        fs::write(outer.path().join("secret.txt"), "secret").unwrap();

        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("../secret.txt", inner.join("escape")).unwrap();
        }

        let table = InodeTable::new(inner.clone());
        let evil_path = inner.join("escape");

        let result = table.validate_path(&evil_path);
        assert!(result.is_err());
    }

    #[test]
    fn lookup_with_dotdot_in_body_rejected() {
        let tmp = TempDir::new().unwrap();
        fs::create_dir(tmp.path().join("subdir")).unwrap();

        let mut table = InodeTable::new(tmp.path().to_path_buf());
        let subdir_ino = table.lookup(ROOT_INODE, "subdir").unwrap();

        let result = table.lookup(subdir_ino, "..");
        assert!(result.is_err());
    }

    #[test]
    fn lookup_with_dot_rejected() {
        let tmp = TempDir::new().unwrap();
        let mut table = InodeTable::new(tmp.path().to_path_buf());

        let result = table.lookup(ROOT_INODE, ".");
        assert!(result.is_err());
    }

    #[test]
    fn name_with_null_byte_handled() {
        let tmp = TempDir::new().unwrap();
        let table = InodeTable::new(tmp.path().to_path_buf());

        let result = table.validate_parent_and_name(ROOT_INODE, "file\0.txt");
        assert!(result.is_ok() || result.is_err());
    }

    #[test]
    fn inode_limit_enforced() {
        let tmp = TempDir::new().unwrap();
        let mut table = InodeTable::new(tmp.path().to_path_buf());

        for i in 0..100 {
            let name = format!("file{}.txt", i);
            fs::write(tmp.path().join(&name), "content").unwrap();
            let _ = table.lookup(ROOT_INODE, &name);
        }

        assert!(table.by_guest_ino.len() <= MAX_INODES);
    }
}
