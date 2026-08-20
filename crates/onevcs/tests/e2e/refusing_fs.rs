//! A filesystem that takes a directory and will not give it back.
//!
//! One thing `onevcs sweep` has to answer for is a directory an entry may be added
//! to and not taken out of again: an append-only attribute, an NFSv4 ACL without
//! `DELETE_CHILD`, a network filesystem that refuses the unlink. An ordinary user
//! can build none of those in a scratch directory — `chattr +a` answers "Operation
//! not permitted" and the rest need a server or a privilege — and every one of them
//! is a *filesystem* answering. So this is one: a mount whose `mkdir` succeeds and
//! whose `rmdir` is refused. Nothing about the tool is stood in for; the kernel
//! routes the real binary's real calls here, and the journey asserts what the sweep
//! decided when a real directory would not give an entry back.
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
    BackgroundSession, FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyDirectory,
    ReplyEmpty, ReplyEntry, Request, TimeOrNow,
};

/// The inode a mount's own root always has.
const ROOT: u64 = 1;

/// Nothing is cached, so every call a journey is about reaches this filesystem
/// rather than an answer the kernel kept from the last one.
const NOW: Duration = Duration::from_secs(0);

/// What an unprivileged mount goes through, and what a host without it is told.
const MOUNTS: &str = "fusermount3";

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
pub fn mount_over(path: &Path, aged: SystemTime) -> BackgroundSession {
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
    fuser::spawn_mount2(Refusing::new(aged), path, &options).unwrap_or_else(|e| {
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
}

impl Refusing {
    fn new(aged: SystemTime) -> Self {
        let mut filesystem = Self {
            attrs: HashMap::new(),
            children: HashMap::new(),
            next: ROOT + 1,
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
        kind: FileType::Directory,
        perm: 0o755,
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
        if !self.children.contains_key(&parent) {
            reply.error(libc::ENOENT);
            return;
        }
        let ino = self.next;
        self.next += 1;
        let attr = directory(ino, SystemTime::now());
        self.attrs.insert(ino, attr);
        self.children.insert(ino, Vec::new());
        self.children
            .entry(parent)
            .or_default()
            .push((name.to_owned(), ino));
        reply.entry(&NOW, &attr, 0);
    }

    /// The whole of what this filesystem is: an entry it took and will not give back,
    /// which is what an append-only directory answers.
    fn rmdir(&mut self, _req: &Request<'_>, _parent: u64, _name: &OsStr, reply: ReplyEmpty) {
        reply.error(libc::EPERM);
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
        let mut listed = vec![(ino, OsString::from(".")), (ROOT, OsString::from(".."))];
        listed.extend(children.iter().map(|(name, child)| (*child, name.clone())));
        for (index, (child, name)) in listed
            .into_iter()
            .enumerate()
            .skip(usize::try_from(offset).unwrap_or(0))
        {
            let next = i64::try_from(index + 1).expect("a directory of this size");
            if reply.add(child, next, FileType::Directory, name) {
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
