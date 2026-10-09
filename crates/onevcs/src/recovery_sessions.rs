//! Disposable session hints for narrowing recovery, never cached session state.
//!
//! Every changed/new source is validated before selection. The records a selection
//! can be answered from are read fresh, so owners, retry links and labels remain
//! live inputs; a record no row of the selection can read is never opened.

#[cfg(unix)]
mod unix {
    use std::collections::{BTreeMap, BTreeSet};
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};

    use crate::error::{self, Result};
    use crate::session::Selection;
    use crate::workspace::{self, Record, Ref, Token};

    /// The shape of the document below. A document of any other is rebuilt.
    const VERSION: u32 = 2;

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
    /// What one record says about where a selection's rows are read, held to the
    /// file it was read from.
    #[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Hint {
        stamp: Stamp,
        token: Token,
        identity: String,
        branch: Ref,
        labels: BTreeMap<String, String>,
        clone: PathBuf,
        execution_checkout: PathBuf,
    }
    impl Hint {
        fn valid(&self, token: &str, stamp: &Stamp) -> bool {
            self.token.to_string() == token
                && self.stamp == *stamp
                && crate::label::validate(&self.labels).is_ok()
        }
        fn of(record: &Record, stamp: Stamp) -> Self {
            Self {
                stamp,
                token: record.token.clone(),
                identity: record.identity.clone(),
                branch: record.branch.clone(),
                labels: record.labels.clone(),
                clone: record.clone.clone(),
                execution_checkout: record.execution_checkout.clone(),
            }
        }
    }

    /// The whole document, held to one digest of its own text: a hint that was
    /// corrupted into another valid one would narrow a selection away from its rows.
    #[derive(Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Index {
        version: u32,
        directory: PathBuf,
        hints: BTreeMap<String, Hint>,
    }

    fn load_index(path: &Path, directory: &Path) -> BTreeMap<String, Hint> {
        let Some(raw) = std::fs::read_to_string(path).ok() else {
            return BTreeMap::new();
        };
        let Some((digest, body)) = raw.split_once('\n') else {
            return BTreeMap::new();
        };
        if crate::ids::digest(body) != digest {
            return BTreeMap::new();
        }
        match serde_json::from_str::<Index>(body) {
            Ok(index) if index.version == VERSION && index.directory == directory => index.hints,
            _ => BTreeMap::new(),
        }
    }

    fn store_index(path: &Path, directory: &Path, hints: BTreeMap<String, Hint>) {
        let Some(parent) = path.parent() else {
            return;
        };
        let index = Index {
            version: VERSION,
            directory: directory.to_owned(),
            hints,
        };
        let Ok(body) = serde_json::to_string(&index) else {
            return;
        };
        let staged = parent.join(format!(".sessions-index.{}.tmp", crate::ids::unique()));
        let _ = std::fs::create_dir_all(parent)
            .and_then(|()| {
                std::fs::write(&staged, format!("{}\n{body}", crate::ids::digest(&body)))
            })
            .and_then(|()| std::fs::rename(&staged, path));
        let _ = std::fs::remove_file(staged);
    }

    /// The records a selection's rows can be read from, in token order.
    ///
    /// Every row a selection answers is a branch some selected session names, and
    /// every reader below `vcs::collected` asks the records it is lent about one of
    /// three things: whether a token names a record at all, the records of one of
    /// those branches by `(identity, branch)` — or, for a full row's prose, by branch
    /// name inside the selected identities — and the records whose clone is one of
    /// the checkouts the selected sessions can be holding a branch in. So those are
    /// the records read, each of them fresh; a record that is none of the three
    /// cannot change a row, and is held only to its hint. Which records those are
    /// is decided from hints held to each file's own stamp, so a record that changed
    /// is read — and refused if it is unreadable — before anything is narrowed away.
    pub(crate) fn read(selection: &Selection) -> Result<Vec<Record>> {
        use std::os::unix::fs::MetadataExt;
        if selection.is_empty() {
            return workspace::all();
        }
        let directory = crate::home::sessions_dir()?;
        let path = crate::home::root()?.join("cache/recoverable/v1/sessions-index.json");
        let old = load_index(&path, &directory);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return workspace::all(),
        };
        let mut known: Vec<(String, Hint)> = Vec::new();
        let mut changed: Vec<(String, Stamp)> = Vec::new();
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
            match old.get(&token).filter(|hint| hint.valid(&token, &stamp)) {
                Some(hint) => known.push((token, hint.clone())),
                None => changed.push((token, stamp)),
            }
        }
        // Raw validation precedes narrowing, including unrelated records. Each is
        // its own file, so they are read side by side, and the first refusal in
        // listing order is the one reported, as reading them in turn would.
        let read = crate::vcs::concurrently(&changed, |(token, _)| workspace::load(token));
        let mut loaded = BTreeMap::new();
        let mut fresh = BTreeMap::new();
        for ((token, stamp), record) in changed.into_iter().zip(read) {
            let record = record?;
            fresh.insert(token.clone(), Hint::of(&record, stamp));
            loaded.insert(token, record);
        }
        fresh.extend(known);
        let named = |token: &Token| selection.sessions.iter().any(|asked| **token == *asked.0);
        // The identities read before (a named token's included, so that `narrowed`
        // can tell a known token whose labels do not match from an unknown one), and
        // what the sessions the selection picks name inside them.
        let mut identities = BTreeSet::new();
        let mut branches = BTreeSet::new();
        let mut checkouts = BTreeSet::new();
        for hint in fresh.values() {
            let labelled = crate::label::matches(&hint.labels, &selection.labels);
            if named(&hint.token) || (selection.sessions.is_empty() && labelled) {
                identities.insert(hint.identity.as_str());
            }
            if (selection.sessions.is_empty() || named(&hint.token)) && labelled {
                branches.insert(&*hint.branch);
                checkouts.insert(hint.clone.as_path());
                checkouts.insert(hint.execution_checkout.as_path());
            }
        }
        let wanted: Vec<&String> = fresh
            .iter()
            .filter(|(_, hint)| {
                named(&hint.token)
                    || (identities.contains(hint.identity.as_str())
                        && (branches.contains(&*hint.branch)
                            || checkouts.contains(hint.clone.as_path())))
            })
            .map(|(token, _)| token)
            .collect();
        let unread: Vec<&String> = wanted
            .iter()
            .copied()
            .filter(|token| !loaded.contains_key(*token))
            .collect();
        let mut reread: BTreeMap<&String, Result<Record>> = unread
            .iter()
            .copied()
            .zip(crate::vcs::concurrently(&unread, |token| {
                workspace::load(token)
            }))
            .collect();
        let mut records = Vec::with_capacity(wanted.len());
        for token in wanted {
            records.push(match loaded.remove(token) {
                Some(record) => record,
                None => reread
                    .remove(token)
                    .expect("every unread wanted record was read")?,
            });
        }
        if fresh != old {
            store_index(&path, &directory, fresh);
        }
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
