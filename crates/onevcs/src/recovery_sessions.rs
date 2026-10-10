//! Disposable session hints for narrowing recovery, never cached session state.
//!
//! Every changed/new source is validated before selection. The records a selection
//! can be answered from are read fresh, so owners, retry links and labels remain
//! live inputs; a record no row of the selection can read is never opened.

#[cfg(unix)]
mod unix {
    use std::collections::{BTreeMap, BTreeSet, HashMap};
    use std::path::{Path, PathBuf};

    use serde::{Deserialize, Serialize};

    use crate::error::{self, Result};
    use crate::session::Selection;
    use crate::workspace::{self, Record, Ref, Token};

    /// The shape of the document below. A document of any other is rebuilt.
    const VERSION: u32 = 3;

    /// A record file's identity and contents, as its metadata says: device, inode,
    /// length, mode, owner, group, and both timestamps to the nanosecond.
    type Stamp = (u64, u64, u64, u32, u32, u32, i64, i64, i64, i64);

    /// What one record says about where a selection's rows are read, held to the
    /// file it was read from: its token, its stamp, then its identity, branch, clone,
    /// execution checkout and labels — the values most records share being indexes
    /// into the document's tables.
    type Entry = (Token, Stamp, usize, Ref, PathBuf, usize, usize);

    /// The whole document, held to one digest of its own text: a hint that was
    /// corrupted into another valid one would narrow a selection away from its rows.
    #[derive(Default, Serialize, Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Index {
        version: u32,
        directory: PathBuf,
        identities: Vec<String>,
        checkouts: Vec<PathBuf>,
        labels: Vec<BTreeMap<String, String>>,
        hints: Vec<Entry>,
    }

    impl Index {
        fn read(path: &Path, directory: &Path) -> Self {
            let read = || {
                let raw = std::fs::read_to_string(path).ok()?;
                let (digest, body) = raw.split_once('\n')?;
                if crate::ids::digest(body) != digest {
                    return None;
                }
                let index: Self = serde_json::from_str(body).ok()?;
                let bounded = index
                    .hints
                    .iter()
                    .all(|(_, _, identity, _, _, checkout, labels)| {
                        *identity < index.identities.len()
                            && *checkout < index.checkouts.len()
                            && *labels < index.labels.len()
                    });
                (index.version == VERSION && index.directory == directory && bounded)
                    .then_some(index)
            };
            read().unwrap_or_default()
        }

        fn write(&self, path: &Path) {
            let Some(parent) = path.parent() else {
                return;
            };
            let Ok(body) = serde_json::to_string(self) else {
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
    }

    /// The hints of one read, every shared value interned once.
    #[derive(Default)]
    struct Tables {
        identities: Vec<String>,
        checkouts: Vec<PathBuf>,
        labels: Vec<BTreeMap<String, String>>,
        identity_at: HashMap<String, usize>,
        checkout_at: HashMap<PathBuf, usize>,
        labels_at: HashMap<BTreeMap<String, String>, usize>,
    }

    impl Tables {
        fn of(index: &mut Index) -> Self {
            let mut tables = Self {
                identities: std::mem::take(&mut index.identities),
                checkouts: std::mem::take(&mut index.checkouts),
                labels: std::mem::take(&mut index.labels),
                ..Self::default()
            };
            tables.identity_at = tables
                .identities
                .iter()
                .enumerate()
                .map(|(at, identity)| (identity.clone(), at))
                .collect();
            tables.checkout_at = tables
                .checkouts
                .iter()
                .enumerate()
                .map(|(at, checkout)| (checkout.clone(), at))
                .collect();
            tables.labels_at = tables
                .labels
                .iter()
                .enumerate()
                .map(|(at, labels)| (labels.clone(), at))
                .collect();
            tables
        }

        fn entry(&mut self, stamp: Stamp, record: &Record) -> Entry {
            let identity = *self
                .identity_at
                .entry(record.identity.clone())
                .or_insert_with(|| {
                    self.identities.push(record.identity.clone());
                    self.identities.len() - 1
                });
            let checkout = *self
                .checkout_at
                .entry(record.execution_checkout.clone())
                .or_insert_with(|| {
                    self.checkouts.push(record.execution_checkout.clone());
                    self.checkouts.len() - 1
                });
            let labels = *self
                .labels_at
                .entry(record.labels.clone())
                .or_insert_with(|| {
                    self.labels.push(record.labels.clone());
                    self.labels.len() - 1
                });
            (
                record.token.clone(),
                stamp,
                identity,
                record.branch.clone(),
                record.clone.clone(),
                checkout,
                labels,
            )
        }
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
        let mut index = Index::read(&path, &directory);
        let entries = match std::fs::read_dir(&directory) {
            Ok(entries) => entries,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(_) => return workspace::all(),
        };
        let mut listed: Vec<(Token, std::fs::DirEntry)> = Vec::new();
        for entry in entries {
            let entry = entry.map_err(error::at("list the session records in", &directory))?;
            let Some(name) = entry
                .file_name()
                .to_string_lossy()
                .strip_suffix(".json")
                .map(str::to_owned)
            else {
                continue;
            };
            // A record's name is its token, held to the spelling one has before it
            // names anything; one that is not is refused as loading it refuses it.
            match Token::try_from(name.clone()) {
                Ok(token) => listed.push((token, entry)),
                Err(_) => {
                    workspace::load(&name)?;
                }
            }
        }
        // Each record is its own file, so they are stamped side by side.
        let stamped = crate::vcs::concurrently(&listed, |(_, entry)| {
            entry.metadata().ok().map(|metadata| {
                (
                    metadata.dev(),
                    metadata.ino(),
                    metadata.len(),
                    metadata.mode(),
                    metadata.uid(),
                    metadata.gid(),
                    metadata.mtime(),
                    metadata.mtime_nsec(),
                    metadata.ctime(),
                    metadata.ctime_nsec(),
                )
            })
        });
        let Some(stamps) = stamped.into_iter().collect::<Option<Vec<Stamp>>>() else {
            return workspace::all();
        };
        let mut tables = Tables::of(&mut index);
        let well_labelled: Vec<bool> = tables
            .labels
            .iter()
            .map(|labels| crate::label::validate(labels).is_ok())
            .collect();
        let mut known: HashMap<String, Entry> = std::mem::take(&mut index.hints)
            .into_iter()
            .map(|entry| (entry.0.to_string(), entry))
            .collect();
        let remembered = known.len();
        let mut fresh: Vec<Entry> = Vec::with_capacity(listed.len());
        let mut changed: Vec<(Token, Stamp)> = Vec::new();
        for ((token, _), stamp) in listed.iter().zip(stamps) {
            match known
                .remove(&**token)
                .filter(|entry| entry.1 == stamp && well_labelled[entry.6])
            {
                Some(entry) => fresh.push(entry),
                None => changed.push((token.clone(), stamp)),
            }
        }
        // Raw validation precedes narrowing, including unrelated records. Each is
        // its own file, so they are read side by side, and the first refusal in
        // listing order is the one reported, as reading them in turn would.
        let read = crate::vcs::concurrently(&changed, |(token, _)| workspace::load(token));
        let moved = !changed.is_empty() || fresh.len() != remembered;
        let mut loaded = HashMap::new();
        for ((token, stamp), record) in changed.into_iter().zip(read) {
            let record = record?;
            fresh.push(tables.entry(stamp, &record));
            loaded.insert(token.to_string(), record);
        }
        fresh.sort_unstable_by(in_token_order);
        let named = |token: &str| selection.sessions.iter().any(|asked| *asked.0 == *token);
        let labelled: Vec<bool> = tables
            .labels
            .iter()
            .map(|labels| crate::label::matches(labels, &selection.labels))
            .collect();
        // The identities read before (a named token's included, so that `narrowed`
        // can tell a known token whose labels do not match from an unknown one), and
        // what the sessions the selection picks name inside them.
        let mut identities = BTreeSet::new();
        let mut branches = BTreeSet::new();
        let mut checkouts = BTreeSet::new();
        for (token, _, identity, branch, clone, checkout, labels) in &fresh {
            let asked = named(token);
            if asked || (selection.sessions.is_empty() && labelled[*labels]) {
                identities.insert(*identity);
            }
            if (selection.sessions.is_empty() || asked) && labelled[*labels] {
                branches.insert(&**branch);
                checkouts.insert(clone.as_path());
                checkouts.insert(tables.checkouts[*checkout].as_path());
            }
        }
        let wanted: Vec<&str> = fresh
            .iter()
            .filter(|(token, _, identity, branch, clone, _, _)| {
                named(token)
                    || (identities.contains(identity)
                        && (branches.contains(&**branch) || checkouts.contains(clone.as_path())))
            })
            .map(|(token, ..)| &**token)
            .collect();
        let unread: Vec<&str> = wanted
            .iter()
            .copied()
            .filter(|token| !loaded.contains_key(*token))
            .collect();
        let mut reread: BTreeMap<&str, Result<Record>> = unread
            .iter()
            .copied()
            .zip(crate::vcs::concurrently(&unread, |token| {
                workspace::load(token)
            }))
            .collect();
        let mut records = Vec::with_capacity(wanted.len());
        for token in &wanted {
            records.push(match loaded.remove(*token) {
                Some(record) => record,
                None => reread
                    .remove(token)
                    .expect("every unread wanted record was read")?,
            });
        }
        if moved {
            store(&path, &directory, &tables, &fresh);
        }
        Ok(records)
    }

    /// Two hints in the order their tokens sort, which is the order records are read in.
    fn in_token_order(left: &Entry, right: &Entry) -> std::cmp::Ordering {
        (*left.0).cmp(&*right.0)
    }

    /// Write the hints with only the table values they still use.
    fn store(path: &Path, directory: &Path, tables: &Tables, fresh: &[Entry]) {
        let mut index = Index {
            version: VERSION,
            directory: directory.to_owned(),
            ..Index::default()
        };
        let mut identities: HashMap<usize, usize> = HashMap::new();
        let mut checkouts: HashMap<usize, usize> = HashMap::new();
        let mut labels: HashMap<usize, usize> = HashMap::new();
        for (token, stamp, identity, branch, clone, checkout, labelled) in fresh {
            let identity = *identities.entry(*identity).or_insert_with(|| {
                index.identities.push(tables.identities[*identity].clone());
                index.identities.len() - 1
            });
            let checkout = *checkouts.entry(*checkout).or_insert_with(|| {
                index.checkouts.push(tables.checkouts[*checkout].clone());
                index.checkouts.len() - 1
            });
            let labelled = *labels.entry(*labelled).or_insert_with(|| {
                index.labels.push(tables.labels[*labelled].clone());
                index.labels.len() - 1
            });
            index.hints.push((
                token.clone(),
                *stamp,
                identity,
                branch.clone(),
                clone.clone(),
                checkout,
                labelled,
            ));
        }
        index.write(path);
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
