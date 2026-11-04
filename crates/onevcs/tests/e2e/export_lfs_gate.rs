//! Hold a real Git LFS media read in the kernel until cancellation has answered.
//! The filesystem exposes one ordinary file; it substitutes neither Git nor LFS.

use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use fuser::{
    BackgroundSession, Config, Errno, FileHandle, FileType, Filesystem, FopenFlags, Generation,
    INodeNo, LockOwner, MountOption, OpenFlags, ReplyAttr, ReplyData, ReplyEntry, ReplyOpen,
    Request,
};

const FILE: INodeNo = INodeNo(2);

#[derive(Default)]
struct Pending {
    pid: AtomicU32,
    released: AtomicBool,
    replies: Mutex<Vec<ReplyData>>,
}

pub(super) struct HeldRead {
    pending: Arc<Pending>,
    mounted: Option<BackgroundSession>,
}

impl HeldRead {
    /// Missing FUSE is a platform limitation of this cancellation journey only.
    pub(super) fn mount(directory: &Path, filename: &str, size: u64) -> Option<Self> {
        if !Path::new("/dev/fuse").exists()
            || !std::process::Command::new("fusermount3")
                .arg("--version")
                .output()
                .is_ok_and(|output| output.status.success())
        {
            eprintln!(
                "skipping actual Git LFS upload cancellation: /dev/fuse or fusermount3 unavailable"
            );
            return None;
        }
        let pending = Arc::new(Pending::default());
        let filesystem = Media {
            filename: filename.into(),
            size,
            pending: pending.clone(),
        };
        let mut config = Config::default();
        config.mount_options = vec![MountOption::FSName("onevcs-held-lfs-media".into())];
        match fuser::spawn_mount(filesystem, directory, &config) {
            Ok(mounted) => Some(Self {
                pending,
                mounted: Some(mounted),
            }),
            Err(error) => {
                eprintln!(
                    "skipping actual Git LFS upload cancellation: FUSE mount unavailable: {error}"
                );
                None
            }
        }
    }

    pub(super) fn reader_pid(&self) -> Option<u32> {
        match self.pending.pid.load(Ordering::Acquire) {
            0 => None,
            pid => Some(pid),
        }
    }

    pub(super) fn release(&self) {
        self.pending.released.store(true, Ordering::Release);
        for reply in self
            .pending
            .replies
            .lock()
            .expect("pending reads")
            .drain(..)
        {
            reply.error(Errno::EINTR);
        }
    }

    pub(super) fn finish(mut self) {
        self.release();
        self.mounted
            .take()
            .expect("mounted")
            .umount_and_join()
            .expect("held LFS filesystem unmounts");
    }
}

impl Drop for HeldRead {
    fn drop(&mut self) {
        self.release();
    }
}

struct Media {
    filename: OsString,
    size: u64,
    pending: Arc<Pending>,
}

impl Media {
    fn attr(&self, ino: INodeNo) -> Option<fuser::FileAttr> {
        let kind = if ino == INodeNo::ROOT {
            FileType::Directory
        } else if ino == FILE {
            FileType::RegularFile
        } else {
            return None;
        };
        let mut attr = crate::refusing_fs::entry(ino, SystemTime::UNIX_EPOCH, kind, 0o755);
        if ino == FILE {
            attr.size = self.size;
            attr.perm = 0o444;
        }
        Some(attr)
    }
}

impl Filesystem for Media {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        if parent == INodeNo::ROOT && name == self.filename {
            reply.entry(
                &Duration::ZERO,
                &self.attr(FILE).expect("file"),
                Generation(0),
            );
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        if let Some(attr) = self.attr(ino) {
            reply.attr(&Duration::ZERO, &attr);
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn open(&self, _req: &Request, ino: INodeNo, _flags: OpenFlags, reply: ReplyOpen) {
        if ino == FILE {
            reply.opened(FileHandle(0), FopenFlags::FOPEN_DIRECT_IO);
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn read(
        &self,
        req: &Request,
        _ino: INodeNo,
        _fh: FileHandle,
        _offset: u64,
        _size: u32,
        _flags: OpenFlags,
        _lock_owner: Option<LockOwner>,
        reply: ReplyData,
    ) {
        let mut replies = self.pending.replies.lock().expect("pending reads");
        if self.pending.released.load(Ordering::Acquire) {
            reply.error(Errno::EINTR);
        } else {
            replies.push(reply);
            self.pending.pid.store(req.pid(), Ordering::Release);
        }
    }
}
