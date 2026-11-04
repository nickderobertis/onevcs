//! The spec-v1 pointer grammar shared by export and branch publication.

pub(crate) const MAX_BYTES: usize = 1024;

use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// A lowercase SHA-256 object identifier, validated on construction.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Sha256Oid(String);

impl Sha256Oid {
    fn parse(value: &str) -> Option<Self> {
        (value.len() == 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()))
        .then(|| Self(value.to_owned()))
    }

    pub(crate) fn digest(bytes: &[u8]) -> Self {
        Self(format!("{:x}", Sha256::digest(bytes)))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }

    pub(crate) fn object_path(&self, store: &Path) -> PathBuf {
        store.join(&self.0[..2]).join(&self.0[2..4]).join(&self.0)
    }
}

impl std::fmt::Display for Sha256Oid {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

pub(crate) struct Pointer {
    oid: Sha256Oid,
    size: u64,
}

impl Pointer {
    pub(crate) fn oid(&self) -> &Sha256Oid {
        &self.oid
    }

    pub(crate) fn size(&self) -> u64 {
        self.size
    }
}

pub(crate) fn parse(bytes: &[u8]) -> Option<Pointer> {
    if bytes.len() > MAX_BYTES {
        return None;
    }
    let text = std::str::from_utf8(bytes).ok()?;
    let mut lines = text.lines();
    if lines.next()? != "version https://git-lfs.github.com/spec/v1" {
        return None;
    }
    let oid = Sha256Oid::parse(lines.next()?.strip_prefix("oid sha256:")?)?;
    let size = lines.next()?.strip_prefix("size ")?.parse().ok()?;
    if lines.next().is_some() {
        return None;
    }
    Some(Pointer { oid, size })
}
