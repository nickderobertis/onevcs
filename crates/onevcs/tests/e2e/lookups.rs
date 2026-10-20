//! A filesystem of this suite's own that counts every name looked up in it.
//!
//! What a repository resolution costs is not a file it opens or a process it spawns:
//! it is a path walked — `canonicalize` of the spelling a caller passed, and of an
//! identity key that may be a relative path — which no `inotify` watch and no `git` on
//! `PATH` sees. A lookup does: each component the kernel resolves under a FUSE mount
//! that caches nothing reaches the mount as one call. So this mount holds one symlink
//! to a real checkout, a caller spells the repository through it, and the process's
//! working directory is the mount while the read runs; each resolution of the spelling
//! is then a lookup of the link's name, and each resolution of a relative identity key
//! a lookup of its first component.
//!
//! Linux only, and refused rather than skipped where it cannot mount, for the reasons
//! `refusing_fs` gives.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime};

use fuser::{
    BackgroundSession, Config, Errno, FileAttr, FileHandle, FileType, Filesystem, Generation,
    INodeNo, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry, Request,
};

use crate::refusing_fs::{assert_mountable, entry};

/// Nothing is cached, so every walk a journey is about reaches this filesystem.
const NOW: Duration = Duration::from_secs(0);

/// The one entry, which is never recycled.
const LINK: INodeNo = INodeNo(INodeNo::ROOT.0 + 1);

/// Every name looked up in the mount's root, and how many times.
type Counted = Arc<Mutex<HashMap<OsString, usize>>>;

/// A mounted root holding one symlink, counting the names looked up in it.
pub struct Lookups {
    session: Option<BackgroundSession>,
    counted: Counted,
    root: PathBuf,
}

impl Lookups {
    /// Mount at `root`, an empty directory, a root holding `link` → `target`.
    pub fn mount(root: &Path, link: &str, target: &Path) -> Self {
        assert_mountable();
        let counted = Counted::default();
        let mut options = Config::default();
        options.mount_options = vec![MountOption::FSName("onevcs-counts-lookups".to_owned())];
        let session = fuser::spawn_mount(
            Counting {
                link: OsString::from(link),
                target: target.as_os_str().as_bytes().to_vec(),
                counted: Arc::clone(&counted),
            },
            root,
            &options,
        )
        .unwrap_or_else(|e| panic!("the counting filesystem mounts at {}: {e}", root.display()));
        Self {
            session: Some(session),
            counted,
            root: root.to_path_buf(),
        }
    }

    /// The path a caller spells the target by, through the link.
    pub fn through(&self, link: &str) -> PathBuf {
        self.root.join(link)
    }

    /// Run `read` with the mount as this process's working directory, and answer how
    /// many times each name was looked up in it while `read` ran.
    pub fn counting<T>(&self, read: impl FnOnce() -> T) -> (T, HashMap<OsString, usize>) {
        let was = std::env::current_dir().expect("a working directory");
        self.counted.lock().expect("the counts").clear();
        std::env::set_current_dir(&self.root).expect("the mount is a directory");
        let answered = read();
        std::env::set_current_dir(was).expect("the working directory is put back");
        let counted = std::mem::take(&mut *self.counted.lock().expect("the counts"));
        (answered, counted)
    }
}

impl Drop for Lookups {
    /// Unmounted and joined, so a mount this suite could not take down is a failure
    /// somebody sees — except while a journey's own panic unwinds, which is the
    /// failure already being reported.
    fn drop(&mut self) {
        if let Some(session) = self.session.take() {
            let unmounted = session.umount_and_join();
            if !std::thread::panicking() {
                unmounted.unwrap_or_else(|e| {
                    panic!(
                        "the counting filesystem unmounts from {}: {e}",
                        self.root.display()
                    )
                });
            }
        }
    }
}

struct Counting {
    link: OsString,
    target: Vec<u8>,
    counted: Counted,
}

impl Counting {
    fn root() -> FileAttr {
        entry(INodeNo::ROOT, SystemTime::now(), FileType::Directory, 0o755)
    }

    fn link(&self) -> FileAttr {
        let mut attr = entry(LINK, SystemTime::now(), FileType::Symlink, 0o777);
        attr.size = u64::try_from(self.target.len()).expect("a path's length");
        attr.nlink = 1;
        attr
    }
}

impl Filesystem for Counting {
    fn lookup(&self, _req: &Request, parent: INodeNo, name: &OsStr, reply: ReplyEntry) {
        if parent == INodeNo::ROOT {
            *self
                .counted
                .lock()
                .expect("the counts")
                .entry(name.to_owned())
                .or_default() += 1;
        }
        if parent == INodeNo::ROOT && name == self.link {
            reply.entry(&NOW, &self.link(), Generation(0));
        } else {
            reply.error(Errno::ENOENT);
        }
    }

    fn getattr(&self, _req: &Request, ino: INodeNo, _fh: Option<FileHandle>, reply: ReplyAttr) {
        match ino {
            INodeNo::ROOT => reply.attr(&NOW, &Self::root()),
            LINK => reply.attr(&NOW, &self.link()),
            _ => reply.error(Errno::ENOENT),
        }
    }

    fn readlink(&self, _req: &Request, ino: INodeNo, reply: ReplyData) {
        match ino {
            LINK => reply.data(&self.target),
            _ => reply.error(Errno::EINVAL),
        }
    }

    fn readdir(
        &self,
        _req: &Request,
        ino: INodeNo,
        _fh: FileHandle,
        offset: u64,
        mut reply: ReplyDirectory,
    ) {
        if ino != INodeNo::ROOT {
            reply.error(Errno::ENOTDIR);
            return;
        }
        let listed = [
            (INodeNo::ROOT, FileType::Directory, OsString::from(".")),
            (INodeNo::ROOT, FileType::Directory, OsString::from("..")),
            (LINK, FileType::Symlink, self.link.clone()),
        ];
        for (index, (child, kind, name)) in listed
            .into_iter()
            .enumerate()
            .skip(usize::try_from(offset).unwrap_or(usize::MAX))
        {
            if reply.add(child, u64::try_from(index + 1).expect("three"), kind, name) {
                break;
            }
        }
        reply.ok();
    }
}
