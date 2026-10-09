// llmlint: ignore[new_code_lands_in_a_project] AGENTS.md assigns onevcs-testing
// to onevcs's workspace targets through crateSource (crates/**/*), as its existing
// sources are owned; a second crate project would duplicate those workspace checks.
//! The public boundary's budget workload: one host the release-binary journeys time
//! the publication term check and an export over.
//!
//! Twenty registered private repositories, each committing a manifest and a term
//! declaration that derive exactly `TERMS_PER_IDENTITY` terms, and one registered
//! public repository with a branch adding `PATHS` files of neutral UTF-8 text,
//! `BYTES` in all. One of the private repositories carries the same files on a
//! branch, confined to one directory, for an export. Everything is built with real
//! git — `fast-import` for the large commits — and the registry is written through
//! onevcs's own test-support bridge. Nothing here matches, diffs, derives or exports:
//! the journeys drive the release binary for all of that.
//!
//! Every owner, name and term is synthetic.

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use onevcs::registry::{Checkout, Identity, Registry};
use onevcs::{testing, Error, Result};

/// Registered private repositories in the check's scope.
pub const IDENTITIES: usize = 20;
/// Terms each of them derives: its `owner/name`, its owner, its name, its package,
/// and 96 declared terms.
pub const TERMS_PER_IDENTITY: usize = 100;
/// Paths the publication adds, and the export copies.
pub const PATHS: usize = 1000;
/// Bytes of UTF-8 text across those paths.
pub const BYTES: usize = 10 * 1024 * 1024;

/// The public repository every write lands in.
pub const DESTINATION: &str = "github.com/sample-owner/openwidget";
/// The branch of the public repository that carries the published work.
pub const PUBLICATION_BRANCH: &str = "generic-workload";
/// The branch of the export's source that carries the directory to export.
pub const EXPORT_BRANCH: &str = "generic-examples";
/// The directory the export copies.
pub const EXPORT_DIRECTORY: &str = "examples";

/// A generated host.
#[derive(Debug)]
pub struct Workload {
    /// The empty root the caller handed over, now holding everything.
    pub root: PathBuf,
    /// The state root to pass as `ONEVCS_HOME`.
    pub home: PathBuf,
    /// The public repository's registered checkout.
    pub destination: PathBuf,
    /// Its bare origin.
    pub destination_origin: PathBuf,
    /// The identity of the private repository the export reads from.
    pub source: String,
    /// The private identities, in order.
    pub identities: Vec<String>,
    /// Bytes of text the publication adds.
    pub bytes: usize,
}

/// The identity key of private repository `index`.
pub fn identity(index: usize) -> String {
    format!("github.com/shelterco{index:02}/quietvault{index:02}")
}

/// Build the workload under `root`, which must be empty or absent.
pub fn build(root: &Path) -> Result<Workload> {
    if root.exists() && std::fs::read_dir(root).map_err(io)?.next().is_some() {
        return Err(invalid(format!(
            "workload root {} must be empty",
            root.display()
        )));
    }
    std::fs::create_dir_all(root).map_err(io)?;
    let root = std::fs::canonicalize(root).map_err(io)?;
    let home = root.join("home");
    std::fs::create_dir_all(&home).map_err(io)?;
    std::fs::write(
        root.join(".gitconfig"),
        "[user]\nname=Fixture\nemail=fixture@example.invalid\n[commit]\ngpgsign=false\n\
         [maintenance]\nauto=false\n[init]\ndefaultBranch=main\n",
    )
    .map_err(io)?;
    let rules = home.join("rules.yml");
    std::fs::write(
        &rules,
        "version: 4\nrules:\n  - match: {host: github.com, owner: \"shelterco*\"}\n    \
         visibility: private\n  - match: {host: github.com, owner: sample-owner, name: \
         openwidget}\n    visibility: public\ndefault: {publication: local-direct, approvals: \
         none}\n",
    )
    .map_err(io)?;

    let files = workload_files();
    let bytes = files.iter().map(|(_, body)| body.len()).sum();
    let mut registry = Registry {
        version: testing::registry_version(),
        identities: BTreeMap::new(),
        checkouts: BTreeMap::new(),
        rules: Some(rules),
    };
    let mut identities = Vec::new();
    for index in 0..IDENTITIES {
        let key = identity(index);
        let checkout = private_repository(&root, index, &files)?;
        registry
            .identities
            .insert(key.clone(), Identity::new(key.clone(), "<no-op>"));
        registry.checkouts.insert(
            format!("quietvault{index:02}"),
            Checkout {
                path: checkout,
                identity: key.clone(),
            },
        );
        identities.push(key);
    }
    let (destination, destination_origin) = public_repository(&root, &files)?;
    registry.identities.insert(
        DESTINATION.to_owned(),
        Identity::new(DESTINATION, "<no-op>"),
    );
    registry.checkouts.insert(
        "openwidget".to_owned(),
        Checkout {
            path: destination.clone(),
            identity: DESTINATION.to_owned(),
        },
    );
    testing::write_registry(&home, &registry)?;
    Ok(Workload {
        root,
        home,
        destination,
        destination_origin,
        source: identity(0),
        identities,
        bytes,
    })
}

/// Words no term is made of, so the generated text is neutral by construction.
const WORDS: [&str; 32] = [
    "amber", "basin", "cedar", "delta", "ember", "fable", "grove", "hazel", "inlet", "jasper",
    "kettle", "lumen", "maple", "nectar", "orbit", "pebble", "quartz", "ripple", "sable",
    "thistle", "umber", "velvet", "willow", "xenon", "yarrow", "zephyr", "anchor", "bramble",
    "canyon", "drift", "estuary", "fern",
];

/// The files the publication adds and the export copies: `PATHS` of them, whose
/// sizes sum to [`BYTES`], each a deterministic run of neutral words.
fn workload_files() -> Vec<(String, String)> {
    let mut state: u64 = 0x5eed_b0a7;
    let per_file = BYTES / PATHS;
    let mut remainder = BYTES % PATHS;
    (0..PATHS)
        .map(|index| {
            let mut size = per_file;
            if remainder > 0 {
                size += 1;
                remainder -= 1;
            }
            let mut text = String::with_capacity(size + 64);
            let mut line = 0;
            while text.len() < size {
                line += 1;
                text.push_str(&format!("line {line}:"));
                for _ in 0..9 {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    text.push(' ');
                    text.push_str(WORDS[(state >> 59) as usize]);
                }
                text.push('\n');
            }
            text.truncate(size - 1);
            text.push('\n');
            (format!("part-{:02}/file-{index:04}.txt", index % 25), text)
        })
        .collect()
}

/// Private repository `index`: a manifest and a declaration committed on `main`, and
/// for the first one a bare origin and the export's branch.
fn private_repository(root: &Path, index: usize, files: &[(String, String)]) -> Result<PathBuf> {
    let name = format!("quietvault{index:02}");
    let repo = root.join("checkouts").join(&name);
    std::fs::create_dir_all(&repo).map_err(io)?;
    git(&repo, &["init", "-q", "-b", "main"])?;
    let declared: Vec<String> = (0..TERMS_PER_IDENTITY - 4)
        .map(|term| format!("\"{name}x{term:03}\""))
        .collect();
    let base = vec![
        ("README.md".to_owned(), "# notes\n".to_owned()),
        (
            "Cargo.toml".to_owned(),
            format!("[package]\nname = \"{name}-core\"\n"),
        ),
        (
            "private-terms.toml".to_owned(),
            format!("schema_version = 1\nterms = [{}]\n", declared.join(", ")),
        ),
    ];
    let mut import = Import::default();
    let main = import.commit("main", None, "chore: the repository", &base);
    if index == 0 {
        let examples: Vec<(String, String)> = files
            .iter()
            .map(|(path, body)| (format!("{EXPORT_DIRECTORY}/{path}"), body.clone()))
            .collect();
        import.commit(
            EXPORT_BRANCH,
            Some(main),
            "docs: add generic examples",
            &examples,
        );
    }
    import.run(&repo)?;
    git(&repo, &["checkout", "-q", "main"])?;
    if index == 0 {
        let origin = root.join("origins").join(format!("{name}.git"));
        git(
            root,
            &[
                "init",
                "-q",
                "--bare",
                "-b",
                "main",
                &origin.to_string_lossy(),
            ],
        )?;
        git(
            &repo,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        )?;
        git(&repo, &["push", "-q", "origin", "main"])?;
        git(&repo, &["fetch", "-q", "origin"])?;
        git(&repo, &["remote", "set-head", "origin", "main"])?;
    }
    Ok(repo)
}

/// The public repository: a bare origin, and a registered checkout of it carrying the
/// publication's branch.
fn public_repository(root: &Path, files: &[(String, String)]) -> Result<(PathBuf, PathBuf)> {
    let origin = root.join("origins/openwidget.git");
    let repo = root.join("checkouts/openwidget");
    std::fs::create_dir_all(&repo).map_err(io)?;
    git(
        root,
        &[
            "init",
            "-q",
            "--bare",
            "-b",
            "main",
            &origin.to_string_lossy(),
        ],
    )?;
    git(&repo, &["init", "-q", "-b", "main"])?;
    let mut import = Import::default();
    let main = import.commit(
        "main",
        None,
        "chore: seed the repository",
        &[("README.md".to_owned(), "# openwidget\n".to_owned())],
    );
    let workload: Vec<(String, String)> = files
        .iter()
        .map(|(path, body)| (format!("workload/{path}"), body.clone()))
        .collect();
    import.commit(
        PUBLICATION_BRANCH,
        Some(main),
        "docs: add generic workload fixtures",
        &workload,
    );
    import.run(&repo)?;
    git(&repo, &["checkout", "-q", "main"])?;
    git(
        &repo,
        &["remote", "add", "origin", &origin.to_string_lossy()],
    )?;
    git(&repo, &["push", "-q", "origin", "main"])?;
    git(&repo, &["fetch", "-q", "origin"])?;
    git(&repo, &["remote", "set-head", "origin", "main"])?;
    git(
        &repo,
        &["branch", "-q", "--set-upstream-to=origin/main", "main"],
    )?;
    Ok((repo, origin))
}

/// A `git fast-import` stream, built in memory.
#[derive(Default)]
struct Import {
    bytes: Vec<u8>,
    next: usize,
}

impl Import {
    fn mark(&mut self) -> usize {
        self.next += 1;
        self.next
    }

    fn commit(
        &mut self,
        branch: &str,
        parent: Option<usize>,
        subject: &str,
        files: &[(String, String)],
    ) -> usize {
        let mut blobs = Vec::new();
        for (path, body) in files {
            let mark = self.mark();
            write!(self.bytes, "blob\nmark :{mark}\ndata {}\n", body.len()).expect("memory");
            self.bytes.extend_from_slice(body.as_bytes());
            self.bytes.push(b'\n');
            blobs.push((path, mark));
        }
        let mark = self.mark();
        write!(
            self.bytes,
            "commit refs/heads/{branch}\nmark :{mark}\ncommitter Fixture \
             <fixture@example.invalid> 1700000000 +0000\ndata {}\n{subject}\n",
            subject.len()
        )
        .expect("memory");
        if let Some(parent) = parent {
            writeln!(self.bytes, "from :{parent}").expect("memory");
        }
        for (path, blob) in blobs {
            writeln!(self.bytes, "M 100644 :{blob} {path}").expect("memory");
        }
        self.bytes.push(b'\n');
        mark
    }

    fn run(self, repo: &Path) -> Result<()> {
        git_input(repo, &["fast-import", "--quiet"], &self.bytes).map(|_| ())
    }
}

fn invalid(reason: impl Into<String>) -> Error {
    Error::Invalid {
        reason: reason.into(),
    }
}

fn io(error: std::io::Error) -> Error {
    invalid(error.to_string())
}

fn git(repo: &Path, args: &[&str]) -> Result<String> {
    git_input(repo, args, &[])
}

fn git_input(repo: &Path, args: &[&str], input: &[u8]) -> Result<String> {
    let home = repo
        .ancestors()
        .find(|ancestor| ancestor.join(".gitconfig").is_file())
        .unwrap_or(repo);
    let mut child = Command::new("git")
        .current_dir(repo)
        .args(args)
        .env_clear()
        .env("PATH", std::env::var_os("PATH").unwrap_or_default())
        .env("HOME", home)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(io)?;
    child
        .stdin
        .take()
        .expect("piped stdin")
        .write_all(input)
        .map_err(io)?;
    let output = child.wait_with_output().map_err(io)?;
    if !output.status.success() {
        return Err(invalid(format!(
            "workload git {} in {}: {}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stderr)
        )));
    }
    String::from_utf8(output.stdout).map_err(|error| invalid(error.to_string()))
}
