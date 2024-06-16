use std::cmp::{max, min};
use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Debug;
use std::fs::{DirEntry, File, OpenOptions, ReadDir};
use std::io::{Cursor, Read, Seek, SeekFrom, Write};
use std::num::{NonZeroUsize, ParseIntError};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::AtomicU64;
use std::sync::{Arc, Weak};
use std::time::{Duration, SystemTime};
use std::{fs, io};

use async_trait::async_trait;
use futures_util::TryStreamExt;
use num_format::{Locale, ToFormattedString};
use rand::thread_rng;
use rand_core::RngCore;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tokio::runtime::Runtime;
use tokio::sync::{Mutex, RwLock};
use tokio::task::{JoinError, JoinSet};
use tokio_stream::wrappers::ReadDirStream;
use tracing::{debug, error, instrument, warn};

use crate::fs_model::{
    CreateFileAttr, DirectoryEntry, DirectoryEntryIterator, DirectoryEntryPlus,
    DirectoryEntryPlusIterator, FileAttr, FileType, FsError, FsResult, SetFileAttr,
};
use crate::stream_util;

#[async_trait]
pub(crate) trait Filesystem: Send + Sync {
    fn exists(&self, ino: u64) -> bool;

    fn is_dir(&self, ino: u64) -> bool;

    fn is_file(&self, ino: u64) -> bool;

    /// Create a new node in the filesystem
    async fn create(
        &self,
        parent: u64,
        name: &str,
        create_attr: CreateFileAttr,
        read: bool,
        write: bool,
    ) -> FsResult<(u64, FileAttr)>;

    async fn find_by_name(&self, parent: u64, name: &str) -> FsResult<Option<FileAttr>>;

    /// Count children of a directory. This **EXCLUDES** "." and "..".
    fn len(&self, ino: u64) -> FsResult<usize>;

    /// Delete a directory
    async fn remove_dir(&self, parent: u64, name: &str) -> FsResult<()>;

    /// Delete a file
    async fn remove_file(&self, parent: u64, name: &str) -> FsResult<()>;

    fn exists_by_name(&self, parent: u64, name: &str) -> FsResult<bool>;

    async fn read_dir(&self, ino: u64) -> FsResult<DirectoryEntryIterator>;

    /// Like [`crate::fs::FilesystemImpl::read_dir`] but with [`FileAttr`] so we don't need to query again for those.
    async fn read_dir_plus(&self, ino: u64) -> FsResult<DirectoryEntryPlusIterator>;

    /// Get metadata
    async fn get_attr(&self, ino: u64) -> FsResult<FileAttr>;

    /// Set metadata
    async fn set_attr(&self, ino: u64, set_attr: SetFileAttr) -> FsResult<()>;

    /// Read the contents from an 'offset'. If we try to read outside of file size, we return 0 bytes.
    /// If the file is not opened for read, it will return an error of type ['FsError::InvalidFileHandle'].
    async fn read(&self, ino: u64, offset: u64, buf: &mut [u8], handle: u64) -> FsResult<usize>;

    async fn release(&self, handle: u64) -> FsResult<()>;

    /// Check if a file is opened for read with this handle.
    async fn is_read_handle(&self, fh: u64) -> bool;

    /// Check if a file is opened for write with this handle.
    async fn is_write_handle(&self, fh: u64) -> bool;

    /// Writes the contents of `buf` to the file at `ino` starting at `offset`.
    /// If we write outside of file size, we fill up with zeros until offset.
    /// If the file is not opened for writing, it will return an error of type ['FsError::InvalidFileHandle'].
    async fn write(&self, ino: u64, offset: u64, buf: &[u8], handle: u64) -> FsResult<usize>;

    /// Flush the data to the underlying storage.
    async fn flush(&self, handle: u64) -> FsResult<()>;

    /// Helpful when we want to copy just some portions of the file.
    async fn copy_file_range(
        &self,
        src_ino: u64,
        src_offset: u64,
        dest_ino: u64,
        dest_offset: u64,
        size: usize,
        src_fh: u64,
        dest_fh: u64,
    ) -> FsResult<usize>;

    /// Open a file. We can open multiple times for read but only one to write at a time.
    async fn open(&self, ino: u64, read: bool, write: bool) -> FsResult<u64>;

    /// Truncates or extends the underlying file, updating the size of this file to become size.
    async fn set_len(&self, ino: u64, size: u64) -> FsResult<()>;

    async fn rename(
        &self,
        parent: u64,
        name: &str,
        new_parent: u64,
        new_name: &str,
    ) -> FsResult<()>;
}

pub(crate) const ROOT_INODE: u64 = 1;

static mut FILENAME: Option<String> = None;

static mut CONTENT: Option<Cursor<Vec<u8>>> = None;

static mut FILE: Option<FileAttr> = None;

static mut ROOT: Option<FileAttr> = None;

/// Encrypted FS that stores encrypted files in a dedicated directory with a specific structure based on `inode`.
pub(crate) struct FilesystemImpl {
    direct_io: bool,
    suid_support: bool,
}

impl FilesystemImpl {
    pub async fn new(direct_io: bool, suid_support: bool) -> FsResult<Arc<Self>> {
        let fs = Self {
            direct_io,
            suid_support,
        };
        fs.ensure_root_exists().await?;
        let arc = Arc::new(fs);
        Ok(arc)
    }

    async fn ensure_root_exists(&self) -> FsResult<()> {
        unsafe {
            FILENAME = Some(String::from_str("hello").unwrap());
            CONTENT = Some(Cursor::new(b"hello world".to_vec()));
            ROOT = Some(FileAttr {
                ino: 1,
                size: 0,
                blocks: 0,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: FileType::Directory,
                perm: 0x755,
                nlink: 1,
                uid: libc::getuid(),
                gid: libc::getgid(),
                rdev: 0,
                blksize: 0,
                flags: 0,
            });
            FILE = Some(FileAttr {
                ino: 42,
                size: 0,
                blocks: 1,
                atime: SystemTime::now(),
                mtime: SystemTime::now(),
                ctime: SystemTime::now(),
                crtime: SystemTime::now(),
                kind: FileType::RegularFile,
                perm: 0o644,
                nlink: 1,
                uid: libc::getuid(),
                gid: libc::getgid(),
                rdev: 0,
                blksize: 0,
                flags: 0,
            });
        }
        Ok(())
    }
}

#[async_trait]
impl Filesystem for FilesystemImpl {
    fn exists(&self, ino: u64) -> bool {
        ino == ROOT_INODE || ino == 42
    }

    fn is_dir(&self, ino: u64) -> bool {
        ino == ROOT_INODE
    }

    fn is_file(&self, ino: u64) -> bool {
        ino == 42
    }

    async fn create(
        &self,
        parent: u64,
        name: &str,
        create_attr: CreateFileAttr,
        read: bool,
        write: bool,
    ) -> FsResult<(u64, FileAttr)> {
        if name == "." || name == ".." {
            return Err(FsError::InvalidInput("name cannot be '.' or '..'"));
        }
        if !self.exists(parent) {
            return Err(FsError::InodeNotFound);
        }
        if self.exists_by_name(parent, name)? {
            return Err(FsError::AlreadyExists);
        }
        Err(FsError::Other("not implemented"))
    }

    async fn find_by_name(&self, parent: u64, name: &str) -> FsResult<Option<FileAttr>> {
        if !self.exists(parent) {
            return Err(FsError::InodeNotFound);
        }
        if !self.is_dir(parent) {
            return Err(FsError::InvalidInodeType);
        }
        return if name == "hello" {
            Ok(Some(file()))
        } else {
            Ok(None)
        };
    }

    fn len(&self, ino: u64) -> FsResult<usize> {
        if !self.is_dir(ino) {
            return Err(FsError::InvalidInodeType);
        }
        return if ino == ROOT_INODE {
            Ok(2)
        } else {
            Err(FsError::InodeNotFound)
        };
    }

    async fn remove_dir(&self, parent: u64, name: &str) -> FsResult<()> {
        if !self.is_dir(parent) {
            return Err(FsError::InvalidInodeType);
        }

        if !self.exists_by_name(parent, name)? {
            return Err(FsError::NotFound("name not found"));
        }

        let attr = self
            .find_by_name(parent, name)
            .await?
            .ok_or(FsError::NotFound("name not found"))?;
        if !matches!(attr.kind, FileType::Directory) {
            return Err(FsError::InvalidInodeType);
        }
        // check if it's empty
        if self.len(attr.ino)? > 0 {
            return Err(FsError::NotEmpty);
        }

        Ok(())
    }

    async fn remove_file(&self, parent: u64, name: &str) -> FsResult<()> {
        if !self.is_dir(parent) {
            return Err(FsError::InvalidInodeType);
        }
        if !self.exists_by_name(parent, name)? {
            return Err(FsError::NotFound("name not found"));
        }

        let attr = self
            .find_by_name(parent, name)
            .await?
            .ok_or(FsError::NotFound("name not found"))?;
        if !matches!(attr.kind, FileType::RegularFile) {
            return Err(FsError::InvalidInodeType);
        }

        Ok(())
    }

    fn exists_by_name(&self, parent: u64, name: &str) -> FsResult<bool> {
        if !self.exists(parent) {
            return Err(FsError::InodeNotFound);
        }
        if !self.is_dir(parent) {
            return Err(FsError::InvalidInodeType);
        }
        unsafe {
            return if name == FILENAME.as_ref().unwrap() {
                Ok(true)
            } else {
                Ok(false)
            };
        }
    }

    async fn read_dir(&self, ino: u64) -> FsResult<DirectoryEntryIterator> {
        if !self.is_dir(ino) {
            return Err(FsError::InvalidInodeType);
        }
        let mut vec = VecDeque::new();
        unsafe {
            vec.push_back(Ok(DirectoryEntry {
                ino: 42,
                name: FILENAME.as_ref().unwrap().to_owned(),
                kind: FileType::RegularFile,
            }));
        }
        Ok(DirectoryEntryIterator(vec))
    }

    async fn read_dir_plus(&self, ino: u64) -> FsResult<DirectoryEntryPlusIterator> {
        if !self.is_dir(ino) {
            return Err(FsError::InvalidInodeType);
        }
        let mut vec = VecDeque::new();
        unsafe {
            vec.push_back(Ok(DirectoryEntryPlus {
                ino: 42,
                name: FILENAME.as_ref().unwrap().to_owned(),
                kind: FileType::RegularFile,
                attr: file(),
            }));
        }
        Ok(DirectoryEntryPlusIterator(vec))
    }

    async fn get_attr(&self, ino: u64) -> FsResult<FileAttr> {
        if !self.exists(ino) {
            return Err(FsError::InodeNotFound);
        }
        if ino == ROOT_INODE {
            unsafe { Ok(*ROOT.as_ref().unwrap()) }
        } else {
            Ok(file())
        }
    }

    async fn set_attr(&self, ino: u64, set_attr: SetFileAttr) -> FsResult<()> {
        if !self.exists(ino) {
            return Err(FsError::InodeNotFound);
        }
        unsafe {
            if self.is_file(ino)
                && set_attr.size.is_some()
                && *set_attr.size.as_ref().unwrap()
                    != CONTENT.as_ref().unwrap().get_ref().len() as u64
            {
                self.set_len(ino, *set_attr.size.as_ref().unwrap()).await?;
            }
            merge_attr(FILE.as_mut().unwrap(), &set_attr);
        }
        Ok(())
    }

    #[instrument(skip(self, buf))]
    async fn read(&self, ino: u64, offset: u64, buf: &mut [u8], handle: u64) -> FsResult<usize> {
        if !self.exists(ino) {
            return Err(FsError::InodeNotFound);
        }
        if !self.is_file(ino) {
            return Err(FsError::InvalidInodeType);
        }
        let attr = file();
        if offset > attr.size {
            return Ok(0);
        }
        let len = min(attr.size - offset, buf.len() as u64) as usize;
        unsafe {
            let content = CONTENT.as_mut().unwrap();
            content.seek(SeekFrom::Start(offset))?;
            content.read_exact(&mut buf[..len])?;
        }
        Ok(len)
    }

    async fn release(&self, handle: u64) -> FsResult<()> {
        Ok(())
    }

    async fn is_read_handle(&self, fh: u64) -> bool {
        true
    }

    async fn is_write_handle(&self, fh: u64) -> bool {
        true
    }

    #[instrument(skip(self, buf))]
    async fn write(&self, ino: u64, offset: u64, buf: &[u8], handle: u64) -> FsResult<usize> {
        if !self.exists(ino) {
            return Err(FsError::InodeNotFound);
        }
        if !self.is_file(ino) {
            return Err(FsError::InvalidInodeType);
        }
        if buf.is_empty() {
            // no-op
            return Ok(0);
        }
        let len = unsafe {
            let content = CONTENT.as_mut().unwrap();
            if offset > content.get_ref().len() as u64 {
                content.seek(SeekFrom::End(0))?;
                stream_util::fill_zeros(content, offset - content.get_ref().len() as u64)?;
                content.write(buf)?
            } else {
                content.seek(SeekFrom::Start(offset))?;
                content.write(buf)?
            }
        };
        Ok(len)
    }

    async fn flush(&self, handle: u64) -> FsResult<()> {
        Ok(())
    }

    async fn copy_file_range(
        &self,
        src_ino: u64,
        src_offset: u64,
        dest_ino: u64,
        dest_offset: u64,
        size: usize,
        src_fh: u64,
        dest_fh: u64,
    ) -> FsResult<usize> {
        if self.is_dir(src_ino) || self.is_dir(dest_ino) {
            return Err(FsError::InvalidInodeType);
        }

        let mut buf = vec![0; size];
        let len = self.read(src_ino, src_offset, &mut buf, src_fh).await?;
        if len == 0 {
            return Ok(0);
        }
        let mut copied = 0;
        while copied < size {
            let len = self
                .write(dest_ino, dest_offset, &buf[copied..len], dest_fh)
                .await?;
            if len == 0 && copied < size {
                error!(len, "Failed to copy all read bytes");
                return Err(FsError::Other("Failed to copy all read bytes"));
            }
            copied += len;
        }
        Ok(len)
    }

    async fn open(&self, ino: u64, read: bool, write: bool) -> FsResult<u64> {
        if !read && !write {
            return Err(FsError::InvalidInput(
                "read and write cannot be false at the same time",
            ));
        }
        if self.is_dir(ino) {
            return Err(FsError::InvalidInodeType);
        }
        Ok(thread_rng().next_u64())
    }

    async fn set_len(&self, ino: u64, size: u64) -> FsResult<()> {
        let attr = self.get_attr(ino).await?;
        if matches!(attr.kind, FileType::Directory) {
            return Err(FsError::InvalidInodeType);
        }

        if size == attr.size {
            // no-op
            return Ok(());
        }

        if size == 0 {
            debug!("truncate to zero");
            // truncate to zero
            unsafe {
                CONTENT = Some(Cursor::new(vec![]));
            }
        } else {
            debug!("truncate size to {}", size.to_formatted_string(&Locale::en));

            let len = if size > attr.size {
                // increase size, copy existing data until existing size
                attr.size
            } else {
                // decrease size, copy existing data until new size
                size
            };
            let mut new_content = Cursor::new(vec![0; size as usize]);
            unsafe {
                let content = CONTENT.as_mut().unwrap();
                content.seek(SeekFrom::Start(0))?;
                stream_util::copy_exact(content, &mut new_content, len)?;
                if size > attr.size {
                    // increase size, seek to new size will write zeros
                    stream_util::fill_zeros(&mut new_content, size - attr.size)?;
                }
                CONTENT = Some(new_content);
            }
        }

        let set_attr = SetFileAttr::default()
            .with_size(size)
            .with_mtime(SystemTime::now())
            .with_ctime(SystemTime::now());
        self.set_attr(ino, set_attr).await?;

        Ok(())
    }

    async fn rename(
        &self,
        parent: u64,
        name: &str,
        new_parent: u64,
        new_name: &str,
    ) -> FsResult<()> {
        if !self.exists(parent) {
            return Err(FsError::InodeNotFound);
        }
        if !self.is_dir(parent) {
            return Err(FsError::InvalidInodeType);
        }
        if !self.exists(new_parent) {
            return Err(FsError::InodeNotFound);
        }
        if !self.is_dir(new_parent) {
            return Err(FsError::InvalidInodeType);
        }
        if !self.exists_by_name(parent, name)? {
            return Err(FsError::NotFound("name not found"));
        }

        if parent == new_parent && name == new_name {
            // no-op
            return Ok(());
        }

        unsafe {
            if parent != ROOT_INODE || new_parent != parent || name != FILENAME.as_ref().unwrap() {
                return Err(FsError::InvalidInput("cannot rename"));
            }
            FILENAME = Some(String::from_str(new_name).unwrap());
        }

        let mut attr = unsafe { FILE.as_mut().unwrap() };

        let mut parent_attr = self.get_attr(parent).await?;
        parent_attr.mtime = SystemTime::now();
        parent_attr.ctime = SystemTime::now();

        attr.ctime = SystemTime::now();

        Ok(())
    }
}

fn merge_attr(attr: &mut FileAttr, set_attr: &SetFileAttr) {
    if let Some(size) = set_attr.size {
        attr.size = size;
    }
    if let Some(atime) = set_attr.atime {
        attr.atime = max(atime, attr.atime);
    }
    if let Some(mtime) = set_attr.mtime {
        attr.mtime = max(mtime, attr.mtime);
    }
    if let Some(ctime) = set_attr.ctime {
        attr.ctime = max(ctime, attr.ctime);
    }
    if let Some(crtime) = set_attr.crtime {
        attr.crtime = max(crtime, attr.crtime);
    }
    if let Some(perm) = set_attr.perm {
        attr.perm = perm;
    }
    if let Some(uid) = set_attr.uid {
        attr.uid = uid;
    }
    if let Some(gid) = set_attr.gid {
        attr.gid = gid;
    }
    if let Some(flags) = set_attr.flags {
        attr.flags = flags;
    }
}

fn file() -> FileAttr {
    unsafe {
        FILE.as_mut().unwrap().size = CONTENT.as_ref().unwrap().get_ref().len() as u64;
        *FILE.as_ref().unwrap()
    }
}
