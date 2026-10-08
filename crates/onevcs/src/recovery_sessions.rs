//! Disposable session hints for narrowing recovery, never cached session state.
//!
//! Every changed/new source is validated before selection. Selected identities'
//! records are read fresh, so owners, retry links and labels remain live inputs.

#[cfg(unix)]
mod unix {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::Path;

    use serde::{Deserialize, Serialize};

    use crate::error::{self, Result};
    use crate::session::Selection;
    use crate::workspace::{self, Record, Ref, Token};

    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Stamp {
        device: u64,
        inode: u64,
        length: u64,
        mode: u32,
        uid: u32,
        gid: u32,
        modified: (i64, i64),
        changed: (i64, i64),
    }
    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Hint {
        stamp: Stamp,
        token: Token,
        identity: String,
        branch: Ref,
        labels: BTreeMap<String, String>,
        checksum: String,
    }
    impl Hint {
        fn checksum(&self, directory: &Path) -> String {
            crate::ids::digest(
                &serde_json::to_string(&(
                    1,
                    directory,
                    &self.stamp,
                    &self.token,
                    &self.identity,
                    &self.branch,
                    &self.labels,
                ))
                .expect("session hint"),
            )
        }
        fn valid(&self, directory: &Path, token: &str, stamp: &Stamp) -> bool {
            self.token.to_string() == token
                && self.stamp == *stamp
                && crate::label::validate(&self.labels).is_ok()
                && self.checksum == self.checksum(directory)
        }
        fn of(record: &Record, directory: &Path, stamp: Stamp) -> Self {
            let mut hint = Self {
                stamp,
                token: record.token.clone(),
                identity: record.identity.clone(),
                branch: record.branch.clone(),
                labels: record.labels.clone(),
                checksum: String::new(),
            };
            hint.checksum = hint.checksum(directory);
            hint
        }
    }

    pub(crate) fn read(selection: &Selection) -> Result<Vec<Record>> {
        use std::os::unix::fs::MetadataExt;
        if selection.is_empty() {
            return workspace::all();
        }
        let directory = crate::home::sessions_dir()?;
        let path = crate::home::root()?.join("cache/recoverable/v1/sessions-index.json");
        let old: BTreeMap<String, Hint> = std::fs::read(&path)
            .ok()
            .and_then(|raw| serde_json::from_slice(&raw).ok())
            .unwrap_or_default();
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return workspace::all(),
        };
        let mut fresh = BTreeMap::new();
        let mut loaded = BTreeMap::new();
        for entry in entries {
            let entry = entry.map_err(error::at("list the session records in", &directory))?;
            let Some(token) = entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".json")
                .map(str::to_owned)
            else {
                continue;
            };
            let metadata = match entry.metadata() {
                Ok(metadata) => metadata,
                Err(_) => return workspace::all(),
            };
            let stamp = Stamp {
                device: metadata.dev(),
                inode: metadata.ino(),
                length: metadata.len(),
                mode: metadata.mode(),
                uid: metadata.uid(),
                gid: metadata.gid(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
            };
            let hint = match old
                .get(&token)
                .filter(|hint| hint.valid(&directory, &token, &stamp))
            {
                Some(hint) => hint.clone(),
                None => {
                    // Raw validation precedes narrowing, including unrelated records.
                    let record = workspace::load(&token)?;
                    let hint = Hint::of(&record, &directory, stamp);
                    loaded.insert(token.clone(), record);
                    hint
                }
            };
            fresh.insert(token, hint);
        }
        let mut identities = BTreeSet::new();
        for hint in fresh.values() {
            let named = selection
                .sessions
                .iter()
                .any(|token| *hint.token == *token.0);
            // A known token with nonmatching labels still has to be read: narrowed()
            // distinguishes that empty selection from an unknown token's refusal.
            if named
                || (selection.sessions.is_empty()
                    && crate::label::matches(&hint.labels, &selection.labels))
            {
                identities.insert(hint.identity.clone());
            }
        }
        let mut records = Vec::new();
        for (token, hint) in &fresh {
            if identities.contains(&hint.identity) {
                records.push(match loaded.remove(token) {
                    Some(record) => record,
                    None => workspace::load(token)?,
                });
            }
        }
        if fresh == old {
            return Ok(records);
        }
        let staged = path
            .parent()
            .expect("cache parent")
            .join(format!(".sessions-index.{}.tmp", crate::ids::unique()));
        if let Ok(bytes) = serde_json::to_vec(&fresh) {
            let _ = std::fs::create_dir_all(path.parent().unwrap())
                .and_then(|()| std::fs::write(&staged, bytes))
                .and_then(|()| std::fs::rename(&staged, &path));
        }
        let _ = std::fs::remove_file(staged);
        Ok(records)
    }
}
#[cfg(unix)]
pub(crate) use unix::read;

#[cfg(not(unix))]
pub(crate) fn read(
    _selection: &crate::session::Selection,
) -> crate::error::Result<Vec<crate::workspace::Record>> {
    crate::workspace::all()
}
