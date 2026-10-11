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

    /// A record file's identity and contents, as its metadata says. Written as one
    /// array in this field order, which keeps a host's worth of hints small.
    #[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
    #[serde(from = "Written", into = "Written")]
    struct Stamp {
        device: u64,
        inode: u64,
        length: u64,
        mode: u32,
        owner: u32,
        group: u32,
        modified: (i64, i64),
        changed: (i64, i64),
    }

    /// A [`Stamp`] as the document holds it: the timestamps as seconds, then
    /// nanoseconds.
    type Written = (u64, u64, u64, u32, u32, u32, i64, i64, i64, i64);

    impl From<Written> for Stamp {
        fn from(
            (device, inode, length, mode, owner, group, mtime, mtime_nsec, ctime, ctime_nsec): Written,
        ) -> Self {
            Self {
                device,
                inode,
                length,
                mode,
                owner,
                group,
                modified: (mtime, mtime_nsec),
                changed: (ctime, ctime_nsec),
            }
        }
    }

    impl From<Stamp> for Written {
        fn from(stamp: Stamp) -> Self {
            (
                stamp.device,
                stamp.inode,
                stamp.length,
                stamp.mode,
                stamp.owner,
                stamp.group,
                stamp.modified.0,
                stamp.modified.1,
                stamp.changed.0,
                stamp.changed.1,
            )
        }
    }

    /// An identity a registry key could be, as the document holds one.
    #[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(try_from = "String", into = "String")]
    struct Identity(String);

    impl Identity {
        /// The identity a session record this read loaded names.
        fn recorded(identity: &str) -> Self {
            Self(identity.to_owned())
        }
    }

    impl TryFrom<String> for Identity {
        type Error = String;

        fn try_from(value: String) -> std::result::Result<Self, Self::Error> {
            if crate::store::is_identity(&value) {
                Ok(Self(value))
            } else {
                Err(format!("{value:?} is not an identity"))
            }
        }
    }

    impl From<Identity> for String {
        fn from(identity: Identity) -> Self {
            identity.0
        }
    }

    /// A checkout or clone path, as the document holds one: absolute, since every
    /// path a session record names is.
    #[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(try_from = "PathBuf", into = "PathBuf")]
    struct Place(PathBuf);

    impl Place {
        /// A path a session record this read loaded names.
        fn recorded(path: &Path) -> Self {
            Self(path.to_owned())
        }
    }

    impl TryFrom<PathBuf> for Place {
        type Error = String;

        fn try_from(value: PathBuf) -> std::result::Result<Self, Self::Error> {
            if value.is_absolute() {
                Ok(Self(value))
            } else {
                Err(format!("{} is not an absolute path", value.display()))
            }
        }
    }

    impl From<Place> for PathBuf {
        fn from(place: Place) -> Self {
            place.0
        }
    }

    /// Where an identity is in the document's `identities`.
    #[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    struct IdentityAt(usize);

    /// Where an execution checkout is in the document's `checkouts`.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    struct CheckoutAt(usize);

    /// Where a set of labels is in the document's `labels`.
    #[derive(Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
    #[serde(transparent)]
    struct LabelsAt(usize);

    /// What one record says about where a selection's rows are read, held to the
    /// file it was read from. The values most records share are places in the
    /// document's tables, which [`Index`] holds in range wherever it reads one.
    #[derive(Clone, Serialize, Deserialize)]
    #[serde(from = "Row", into = "Row")]
    struct Hint {
        token: Token,
        stamp: Stamp,
        identity: IdentityAt,
        branch: Ref,
        clone: Place,
        checkout: CheckoutAt,
        labels: LabelsAt,
    }

    /// A [`Hint`] as the document holds it, in this field order, which keeps a host's
    /// worth of hints small.
    type Row = (Token, Stamp, IdentityAt, Ref, Place, CheckoutAt, LabelsAt);

    impl From<Row> for Hint {
        fn from((token, stamp, identity, branch, clone, checkout, labels): Row) -> Self {
            Self {
                token,
                stamp,
                identity,
                branch,
                clone,
                checkout,
                labels,
            }
        }
    }

    impl From<Hint> for Row {
        fn from(hint: Hint) -> Self {
            (
                hint.token,
                hint.stamp,
                hint.identity,
                hint.branch,
                hint.clone,
                hint.checkout,
                hint.labels,
            )
        }
    }

    /// The whole document, held to one digest of its own text: a hint that was
    /// corrupted into another valid one would narrow a selection away from its rows.
    /// Read only through [`Document`], so every hint's indexes name a table value and
    /// every identity and path in it is one a session record could name.
    #[derive(Default, Serialize, Deserialize)]
    #[serde(try_from = "Document")]
    struct Index {
        version: u32,
        directory: PathBuf,
        identities: Vec<Identity>,
        checkouts: Vec<Place>,
        labels: Vec<BTreeMap<String, String>>,
        hints: Vec<Hint>,
    }

    /// The document as written, before its indexes are held to its tables.
    #[derive(Deserialize)]
    #[serde(deny_unknown_fields)]
    struct Document {
        version: u32,
        directory: PathBuf,
        identities: Vec<Identity>,
        checkouts: Vec<Place>,
        labels: Vec<BTreeMap<String, String>>,
        hints: Vec<Hint>,
    }

    impl TryFrom<Document> for Index {
        type Error = &'static str;

        fn try_from(document: Document) -> std::result::Result<Self, Self::Error> {
            let bounded = document.hints.iter().all(|hint| {
                hint.identity.0 < document.identities.len()
                    && hint.checkout.0 < document.checkouts.len()
                    && hint.labels.0 < document.labels.len()
            });
            // llmlint: ignore[changed_behavior_has_e2e] reached only by a document no
            // verb writes; `labels::session_hints_observe_new_labels_and_refuse_changed_unrelated_records`
            // writes one under a matching digest and holds the binary's rows to the
            // read without it.
            if !bounded {
                return Err("a hint names a table value the document does not hold");
            }
            Ok(Self {
                version: document.version,
                directory: document.directory,
                identities: document.identities,
                checkouts: document.checkouts,
                labels: document.labels,
                hints: document.hints,
            })
        }
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
                (index.version == VERSION && index.directory == directory).then_some(index)
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
        identities: Vec<Identity>,
        checkouts: Vec<Place>,
        labels: Vec<BTreeMap<String, String>>,
        identity_at: HashMap<Identity, usize>,
        checkout_at: HashMap<Place, usize>,
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

        fn hint(&mut self, stamp: Stamp, record: &Record) -> Hint {
            let identity = *self
                .identity_at
                .entry(Identity::recorded(&record.identity))
                .or_insert_with(|| {
                    self.identities.push(Identity::recorded(&record.identity));
                    self.identities.len() - 1
                });
            let checkout = *self
                .checkout_at
                .entry(Place::recorded(&record.execution_checkout))
                .or_insert_with(|| {
                    self.checkouts
                        .push(Place::recorded(&record.execution_checkout));
                    self.checkouts.len() - 1
                });
            let labels = *self
                .labels_at
                .entry(record.labels.clone())
                .or_insert_with(|| {
                    self.labels.push(record.labels.clone());
                    self.labels.len() - 1
                });
            Hint {
                token: record.token.clone(),
                stamp,
                identity: IdentityAt(identity),
                branch: record.branch.clone(),
                clone: Place::recorded(&record.clone),
                checkout: CheckoutAt(checkout),
                labels: LabelsAt(labels),
            }
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
            entry.metadata().ok().map(|metadata| Stamp {
                device: metadata.dev(),
                inode: metadata.ino(),
                length: metadata.len(),
                mode: metadata.mode(),
                owner: metadata.uid(),
                group: metadata.gid(),
                modified: (metadata.mtime(), metadata.mtime_nsec()),
                changed: (metadata.ctime(), metadata.ctime_nsec()),
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
        let mut known: HashMap<String, Hint> = std::mem::take(&mut index.hints)
            .into_iter()
            .map(|hint| (hint.token.to_string(), hint))
            .collect();
        let remembered = known.len();
        let mut fresh: Vec<Hint> = Vec::with_capacity(listed.len());
        let mut changed: Vec<(Token, Stamp)> = Vec::new();
        for ((token, _), stamp) in listed.iter().zip(stamps) {
            match known
                .remove(&**token)
                .filter(|hint| hint.stamp == stamp && well_labelled[hint.labels.0])
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
            fresh.push(tables.hint(stamp, &record));
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
        for hint in &fresh {
            let asked = named(&hint.token);
            let labelled = labelled[hint.labels.0];
            if asked || (selection.sessions.is_empty() && labelled) {
                identities.insert(hint.identity);
            }
            if (selection.sessions.is_empty() || asked) && labelled {
                branches.insert(&*hint.branch);
                checkouts.insert(hint.clone.0.as_path());
                checkouts.insert(tables.checkouts[hint.checkout.0].0.as_path());
            }
        }
        let wanted: Vec<&str> = fresh
            .iter()
            .filter(|hint| {
                named(&hint.token)
                    || (identities.contains(&hint.identity)
                        && (branches.contains(&*hint.branch)
                            || checkouts.contains(hint.clone.0.as_path())))
            })
            .map(|hint| &*hint.token)
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
    fn in_token_order(left: &Hint, right: &Hint) -> std::cmp::Ordering {
        (*left.token).cmp(&*right.token)
    }

    /// Write the hints with only the table values they still use.
    fn store(path: &Path, directory: &Path, tables: &Tables, fresh: &[Hint]) {
        let mut index = Index {
            version: VERSION,
            directory: directory.to_owned(),
            ..Index::default()
        };
        let mut identities: HashMap<IdentityAt, IdentityAt> = HashMap::new();
        let mut checkouts: HashMap<CheckoutAt, CheckoutAt> = HashMap::new();
        let mut labels: HashMap<LabelsAt, LabelsAt> = HashMap::new();
        for hint in fresh {
            let identity = *identities.entry(hint.identity).or_insert_with(|| {
                index
                    .identities
                    .push(tables.identities[hint.identity.0].clone());
                IdentityAt(index.identities.len() - 1)
            });
            let checkout = *checkouts.entry(hint.checkout).or_insert_with(|| {
                index
                    .checkouts
                    .push(tables.checkouts[hint.checkout.0].clone());
                CheckoutAt(index.checkouts.len() - 1)
            });
            let labelled = *labels.entry(hint.labels).or_insert_with(|| {
                index.labels.push(tables.labels[hint.labels.0].clone());
                LabelsAt(index.labels.len() - 1)
            });
            index.hints.push(Hint {
                identity,
                checkout,
                labels: labelled,
                ..hint.clone()
            });
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
