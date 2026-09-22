//! How many times a directory was listed, counted by the kernel rather than by the
//! code under test.
//!
//! The question a journey about quadratic work has to ask is how much of it the
//! binary did, and nothing the binary prints says so: a scan that re-read every
//! session record once per session record answers exactly what a scan that read them
//! once answers, only slower — which is why the defect stood while every journey
//! about the answers stayed green. So the counting is arranged outside the process,
//! in the one place that sees a `read_dir` for what it is: `inotify` watches the
//! session directory's own inode for `IN_OPEN`, and every `opendir` of it — this
//! process's, the spawned binary's, anybody's — arrives as one event.
//!
//! **A real syscall, counted where it lands.** Nothing here substitutes a filesystem
//! or a reader: the journey spawns the real `onevcs`, the binary makes the real calls
//! it always makes, and what the assertion reads is the kernel's own record of them.
//!
//! Linux only, because `inotify` is. `just gate` runs there, and the journeys that
//! use this are gated the same way `refusing_fs`'s are.

#![cfg(target_os = "linux")]

use std::ffi::CString;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::path::{Path, PathBuf};

/// The fixed part of one `struct inotify_event`: `wd`, `mask`, `cookie`, `len`.
const HEADER: usize = 16;

/// Big enough for every event one journey's command can produce before it is drained:
/// the queue's own default bound is 16384 events, and a scan this is used on makes
/// tens.
const BUFFER: usize = 64 * 1024;

/// A watch on one directory, counting the times it is opened for listing.
pub struct Listings {
    watch: OwnedFd,
    directory: PathBuf,
}

impl Listings {
    /// Start counting opens of `directory`, which must already exist.
    ///
    /// Everything the count is about has to happen after this returns, so a journey
    /// takes the watch and then spawns the command — the kernel queues each event
    /// until [`Listings::taken`] drains it.
    pub fn of(directory: &Path) -> Self {
        assert!(
            directory.is_dir(),
            "this counter watches an existing directory, and {} is not one",
            directory.display()
        );
        // SAFETY: no arguments to get wrong; the descriptor is checked below and
        // owned from here on.
        let fd = unsafe { libc::inotify_init1(libc::IN_NONBLOCK) };
        assert!(
            fd >= 0,
            "this host allows an inotify watch: {}",
            std::io::Error::last_os_error()
        );
        // SAFETY: `fd` is a fresh descriptor this call has just been handed and
        // nothing else owns.
        let watch = unsafe { OwnedFd::from_raw_fd(fd) };
        let path = CString::new(directory.as_os_str().as_encoded_bytes())
            .expect("a state-root path holds no NUL");
        // SAFETY: `path` outlives the call, and `watch` is open for its duration.
        let added =
            unsafe { libc::inotify_add_watch(watch.as_raw_fd(), path.as_ptr(), libc::IN_OPEN) };
        assert!(
            added >= 0,
            "this host watches {}: {}",
            directory.display(),
            std::io::Error::last_os_error()
        );
        Self {
            watch,
            directory: directory.to_path_buf(),
        }
    }

    /// How many times the directory itself has been opened since the watch was taken,
    /// draining what has been counted so far.
    ///
    /// The directory *itself*, not the records in it: opening a file under a watched
    /// directory arrives as an event naming that file, and opening the directory
    /// arrives with no name at all. Only the second is a listing, and the first is
    /// what a reader of the records does whichever way it found them.
    pub fn taken(&self) -> usize {
        let mut buffer = vec![0u8; BUFFER];
        let mut listings = 0;
        loop {
            // SAFETY: the buffer is writable for exactly the length passed, and the
            // descriptor is open for the call.
            let read = unsafe {
                libc::read(
                    self.watch.as_raw_fd(),
                    buffer.as_mut_ptr().cast(),
                    buffer.len(),
                )
            };
            if read < 0 {
                let failure = std::io::Error::last_os_error();
                assert_eq!(
                    failure.kind(),
                    std::io::ErrorKind::WouldBlock,
                    "reading the watch on {} failed: {failure}",
                    self.directory.display()
                );
                return listings;
            }
            let read = usize::try_from(read).expect("a non-negative read");
            let mut at = 0;
            while at + HEADER <= read {
                let name = u32::from_ne_bytes(
                    buffer[at + 12..at + HEADER]
                        .try_into()
                        .expect("four bytes of length"),
                );
                let name = usize::try_from(name).expect("a name length this host reported");
                if name == 0 {
                    listings += 1;
                }
                at += HEADER + name;
            }
        }
    }
}
