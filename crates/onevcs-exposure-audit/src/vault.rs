//! The host-local vault the full findings live in.
//!
//! A run gets its own directory under the vault root, named by an opaque id, mode
//! `0700`, with every file `0600`. The root must not be inside a git checkout: the
//! findings quote private terms, and a checkout is one `git add -A` from publishing
//! them.

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Write};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;
use sha2::{Digest, Sha256};

pub struct Vault {
    dir: PathBuf,
}

/// Why a vault could not be opened. Never carries a path or an OS message.
#[derive(Debug)]
pub enum VaultRefusal {
    InsideCheckout,
    /// The part of the path that exists could not be resolved, so where it leads is
    /// unknown.
    Unresolvable,
    Unwritable,
}

/// The default root: `${XDG_STATE_HOME:-$HOME/.local/state}/ai-orchestrator/private-boundary-audit`.
pub fn default_root() -> Option<PathBuf> {
    let state = std::env::var_os("XDG_STATE_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".local/state")))?;
    Some(state.join("ai-orchestrator").join("private-boundary-audit"))
}

/// `path` as the filesystem resolves it: absolute, with every symlink in the part
/// of it that exists followed, and the part that does not yet exist appended
/// lexically. `None` when the existing part cannot be resolved.
pub fn resolve(path: &Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut existing = absolute.as_path();
    let mut missing: Vec<Component<'_>> = Vec::new();
    while fs::symlink_metadata(existing).is_err() {
        missing.extend(existing.components().next_back());
        existing = existing.parent()?;
    }
    let mut resolved = fs::canonicalize(existing).ok()?;
    // Only names and `..` can be missing: the root exists, and `components` drops
    // every `.` but a leading one, which an absolute path does not have.
    for component in missing.into_iter().rev() {
        if component == Component::ParentDir {
            resolved.pop();
        } else {
            resolved.push(component);
        }
    }
    Some(resolved)
}

/// Whether a resolved path, or any ancestor of it, is a git checkout (a work tree or
/// a repository directory). Resolve first: a path spelled outside a checkout can
/// reach into one through a symlink.
pub fn inside_checkout(resolved: &Path) -> bool {
    resolved.ancestors().any(|dir| {
        dir.join(".git").exists() || (dir.join("HEAD").is_file() && dir.join("objects").is_dir())
    })
}

impl Vault {
    pub fn create(root: &Path) -> Result<Vault, VaultRefusal> {
        let root = &resolve(root).ok_or(VaultRefusal::Unresolvable)?;
        if inside_checkout(root) {
            return Err(VaultRefusal::InsideCheckout);
        }
        fs::create_dir_all(root).map_err(|_| VaultRefusal::Unwritable)?;
        set_mode(root, 0o700).map_err(|_| VaultRefusal::Unwritable)?;
        let dir = root.join(opaque_id());
        fs::create_dir(&dir).map_err(|_| VaultRefusal::Unwritable)?;
        set_mode(&dir, 0o700).map_err(|_| VaultRefusal::Unwritable)?;
        Ok(Vault { dir })
    }

    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// A new `0600` file in the run's directory.
    pub fn file(&self, name: &str) -> io::Result<BufWriter<File>> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        Ok(BufWriter::new(options.open(self.dir.join(name))?))
    }

    pub fn write(&self, name: &str, contents: &str) -> io::Result<()> {
        let mut file = self.file(name)?;
        file.write_all(contents.as_bytes())?;
        file.flush()
    }

    /// A `0700` directory in the run's directory, for the temporary clones.
    pub fn subdir(&self, name: &str) -> io::Result<PathBuf> {
        let dir = self.dir.join(name);
        fs::create_dir(&dir)?;
        set_mode(&dir, 0o700)?;
        Ok(dir)
    }
}

/// A JSON-lines stream of findings in the vault, written as they are found.
///
/// A row the vault refuses is not dropped silently: the stream remembers the
/// failure, stops writing, and [`Findings::finish`] reports it, so a run whose report
/// lost a row ends as a failed run rather than as a clean one.
pub struct Findings {
    out: BufWriter<File>,
    rows: u64,
    failed: bool,
}

impl Findings {
    pub fn open(vault: &Vault, name: &str) -> io::Result<Findings> {
        Ok(Findings {
            out: vault.file(name)?,
            rows: 0,
            failed: false,
        })
    }

    pub fn push(&mut self, row: &impl Serialize) {
        if self.failed {
            return;
        }
        let written = serde_json::to_string(row)
            .map_err(io::Error::other)
            .and_then(|line| writeln!(self.out, "{line}"));
        match written {
            Ok(()) => self.rows += 1,
            Err(_) => self.failed = true,
        }
    }

    /// The rows written, or the failure that stopped them.
    pub fn finish(mut self) -> io::Result<u64> {
        if self.failed {
            return Err(io::Error::other("a finding row was refused"));
        }
        self.out.flush()?;
        Ok(self.rows)
    }
}

fn opaque_id() -> String {
    let mut hasher = Sha256::new();
    hasher.update(std::process::id().to_le_bytes());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    hasher.update(now.to_le_bytes());
    let digest = hasher.finalize();
    digest[..8].iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(unix)]
fn set_mode(path: &Path, mode: u32) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))
}

#[cfg(not(unix))]
fn set_mode(_path: &Path, _mode: u32) -> io::Result<()> {
    Ok(())
}
