//! A filesystem of this suite's own that takes an entry and will not give it back.
//!
//! Something is arranged here, and it is the host rather than the tool: a mount whose
//! `rmdir` and `unlink` answer `EPERM`, so a real syscall fails where a journey needs
//! one to. That is what `onevcs sweep` has to answer for — a directory an entry may
//! be added to and not taken out of again — and an ordinary user can provision none
//! of the things that do it: `chattr +a` answers "Operation not permitted", and an
//! NFSv4 ACL without `DELETE_CHILD` or a policy granting `add_name` without
//! `remove_name` needs a server or a privilege a suite cannot have.
//!
//! It is arranged in the kernel and not in the code under test: the call leaves the
//! process, crosses the VFS, and comes back refused, which is indistinguishable from
//! any other mount to the binary making it. What each journey asserts is what the
//! real sweep decided when a real removal failed.
//!
//! Linux only, because that is where an unprivileged mount needs nothing outside the
//! distribution's own `fuse3`. **Absent, it refuses loudly rather than skipping** —
//! a journey that quietly passed when it could not build its own premise is the
//! shape of failure the verb it tests exists to prevent.

use std::collections::HashMap;
use std::ffi::{OsStr, OsString};
use std::path::Path;
use std::process::Command;
use std::time::{Duration, SystemTime};

use fuser::{
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyCreate,
    ReplyDirectory, ReplyEmpty, ReplyEntry, Request, TimeOrNow,
};

/// The inode a mount's own root always has.
const ROOT: u64 = 1;

/// Nothing is cached, so every call a journey is about reaches this filesystem
/// rather than an answer the kernel kept from the last one.
const NOW: Duration = Duration::from_secs(0);

/// What an unprivileged mount goes through, and what a host without it is told.
const MOUNTS: &str = "fusermount3";

/// What a mount will not give back, which is the half of the probe's question the
/// journey mounting it is about.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Refuses {
    /// `rmdir` is refused; a file may still be unlinked.
    Directories,
    /// `unlink` is refused; a directory may still be removed.
    Files,
}

/// Mount one over `path`, with everything on it as old as `aged`.
///
/// The age is the mount's own: this verb's age floor reads a mounted directory like
/// any other, and one that looked freshly written would be kept for its age instead
/// of for the question this journey is about.
///
/// The session unmounts when it is dropped — through `fusermount3 -u`, which is what
/// an unprivileged unmount goes through — and a journey's own panic drops it while it
/// unwinds, so a failing run leaves no mount behind. `auto_unmount` is deliberately
/// not asked for: `fusermount3` allows it only alongside `allow_other`, which needs
/// `user_allow_other` in `/etc/fuse.conf` — a host change a suite must not require.
pub fn mount_over(path: &Path, aged: SystemTime, refuses: Refuses) -> BackgroundSession {
    // Checked before the mount rather than read out of whatever error it fails with:
    // a host missing one package should be told which, not handed a errno.
    let mounts = Command::new(MOUNTS).arg("--version").output();
    assert!(
        mounts.is_ok_and(|ran| ran.status.success()),
        "this journey mounts a filesystem of its own and {MOUNTS} is not on PATH.\n\
         ACTION: install it — on Debian and Ubuntu `sudo apt-get install -y fuse3`, on \
         Fedora `sudo dnf install fuse3` — and re-run. This tier refuses rather than \
         skipping: a journey that passed without building its own premise would prove \
         nothing.",
    );
    assert!(
        Path::new("/dev/fuse").exists(),
        "this journey mounts a filesystem of its own and /dev/fuse is not there.\n\
         ACTION: load the `fuse` module (`sudo modprobe fuse`) or run this suite on a \
         host whose container is given /dev/fuse, and re-run.",
    );

    let options = [MountOption::FSName("onevcs-refuses-removal".to_owned())];
    fuser::spawn_mount2(Refusing::new(aged, refuses), path, &options).unwrap_or_else(|e| {
        panic!(
            "this journey's filesystem could not be mounted at {}: {e}\n\
             ACTION: check that {MOUNTS} may mount for this user (`user_allow_other` is \
             not needed) and that /dev/fuse is writable by it.",
            path.display()
        )
    })
}

/// Directories, in memory, that may be made and not removed.
struct Refusing {
    attrs: HashMap<u64, FileAttr>,
    children: HashMap<u64, Vec<(OsString, u64)>>,
    next: u64,
    refuses: Refuses,
}

impl Refusing {
    /// Record one entry under `parent`, or `None` where there is no such directory.
    fn made(&mut self, parent: u64, name: &OsStr, kind: FileType, perm: u16) -> Option<FileAttr> {
        self.children.contains_key(&parent).then(|| {
            let ino = self.next;
            self.next += 1;
            let attr = entry(ino, SystemTime::now(), kind, perm);
            self.attrs.insert(ino, attr);
            if kind == FileType::Directory {
                self.children.insert(ino, Vec::new());
            }
            self.children
                .entry(parent)
                .or_default()
                .push((name.to_owned(), ino));
            attr
        })
    }

    /// Take one entry away, or refuse where that is the kind this mount is about.
    fn take_away(&mut self, parent: u64, name: &OsStr, kind: Refuses, reply: ReplyEmpty) {
        if self.refuses == kind {
            reply.error(libc::EPERM);
            return;
        }
        let Some(entries) = self.children.get_mut(&parent) else {
            reply.error(libc::ENOENT);
            return;
        };
        let Some(at) = entries.iter().position(|(held, _)| held == name) else {
            reply.error(libc::ENOENT);
            return;
        };
        let (_, ino) = entries.remove(at);
        self.attrs.remove(&ino);
        self.children.remove(&ino);
        reply.ok();
    }

    fn new(aged: SystemTime, refuses: Refuses) -> Self {
        let mut filesystem = Self {
            attrs: HashMap::new(),
            children: HashMap::new(),
            next: ROOT + 1,
            refuses,
        };
        filesystem.attrs.insert(ROOT, directory(ROOT, aged));
        filesystem.children.insert(ROOT, Vec::new());
        filesystem
    }
}

/// One directory as the kernel is told about it: this user's, ordinary permissions,
/// and no sticky bit — what a directory hands back to an entry's owner is a separate
/// question the sweep asks, and not the one this filesystem is here to answer.
fn directory(ino: u64, when: SystemTime) -> FileAttr {
    entry(ino, when, FileType::Directory, 0o755)
}

/// One entry of either kind as the kernel is told about it.
fn entry(ino: u64, when: SystemTime, kind: FileType, perm: u16) -> FileAttr {
    // SAFETY: both read this process's own credentials and cannot fail.
    let (uid, gid) = unsafe { (libc::geteuid(), libc::getegid()) };
    FileAttr {
        ino,
        size: 0,
        blocks: 0,
        atime: when,
        mtime: when,
        ctime: when,
        crtime: when,
        kind,
        perm,
        nlink: 2,
        uid,
        gid,
        rdev: 0,
        blksize: 512,
        flags: 0,
    }
}

impl Filesystem for Refusing {
    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        match self
            .children
            .get(&parent)
            .and_then(|entries| entries.iter().find(|(held, _)| held == name))
            .and_then(|(_, ino)| self.attrs.get(ino))
        {
            Some(attr) => reply.entry(&NOW, attr, 0),
            None => reply.error(libc::ENOENT),
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        match self.attrs.get(&ino) {
            Some(attr) => reply.attr(&NOW, attr),
            None => reply.error(libc::ENOENT),
        }
    }

    /// The clock is set, because the probe puts a directory's back before it writes
    /// anything: a mount that refused this would answer the question the journey
    /// beside this one is about rather than its own.
    fn setattr(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _mode: Option<u32>,
        _uid: Option<u32>,
        _gid: Option<u32>,
        _size: Option<u64>,
        atime: Option<TimeOrNow>,
        mtime: Option<TimeOrNow>,
        _ctime: Option<SystemTime>,
        _fh: Option<u64>,
        _crtime: Option<SystemTime>,
        _chgtime: Option<SystemTime>,
        _bkuptime: Option<SystemTime>,
        _flags: Option<u32>,
        reply: ReplyAttr,
    ) {
        let Some(attr) = self.attrs.get_mut(&ino) else {
            reply.error(libc::ENOENT);
            return;
        };
        if let Some(when) = atime {
            attr.atime = instant(when);
        }
        if let Some(when) = mtime {
            attr.mtime = instant(when);
        }
        reply.attr(&NOW, attr);
    }

    fn mkdir(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        reply: ReplyEntry,
    ) {
        match self.made(parent, name, FileType::Directory, 0o755) {
            Some(attr) => reply.entry(&NOW, &attr, 0),
            None => reply.error(libc::ENOENT),
        }
    }

    /// The whole of what this filesystem is: an entry it took and will not give back.
    /// One kind at a time, because `rmdir` and `unlink` are separate rights in an
    /// NFSv4 ACL and separate bits in a Landlock policy, and the probe asks both.
    fn rmdir(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        self.take_away(parent, name, Refuses::Directories, reply);
    }

    fn unlink(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEmpty) {
        self.take_away(parent, name, Refuses::Files, reply);
    }

    /// A file may be made here, so that what refuses is the taking away rather than
    /// the making — the probe asks both, and this answers both the same way.
    fn create(
        &mut self,
        _req: &Request<'_>,
        parent: u64,
        name: &OsStr,
        _mode: u32,
        _umask: u32,
        _flags: i32,
        reply: ReplyCreate,
    ) {
        match self.made(parent, name, FileType::RegularFile, 0o644) {
            Some(attr) => reply.created(&NOW, &attr, 0, 0, 0),
            None => reply.error(libc::ENOENT),
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        let Some(children) = self.children.get(&ino) else {
            reply.error(libc::ENOENT);
            return;
        };
        let kind = |child: u64| {
            self.attrs
                .get(&child)
                .map_or(FileType::Directory, |attr| attr.kind)
        };
        let mut listed = vec![
            (ino, FileType::Directory, OsString::from(".")),
            (ROOT, FileType::Directory, OsString::from("..")),
        ];
        listed.extend(
            children
                .iter()
                .map(|(name, child)| (*child, kind(*child), name.clone())),
        );
        for (index, (child, kind, name)) in listed
            .into_iter()
            .enumerate()
            .skip(usize::try_from(offset).unwrap_or(0))
        {
            let next = i64::try_from(index + 1).expect("a directory of this size");
            if reply.add(child, next, kind, name) {
                break;
            }
        }
        reply.ok();
    }
}

/// A time the kernel asked for, as the instant to record.
fn instant(when: TimeOrNow) -> SystemTime {
    match when {
        TimeOrNow::SpecificTime(at) => at,
        TimeOrNow::Now => SystemTime::now(),
    }
}
