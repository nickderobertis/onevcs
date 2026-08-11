//! The scratch repository, the credential, and the world one real-backend journey
//! runs in.
//!
//! Everything here is about *reaching* the real backends safely. The guard that
//! decides which repository may be touched, the credential probe that fails loudly
//! rather than skipping, the git configuration that authenticates without editing
//! anything outside this run, and the cleanup that removes the branches a run made.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use onevcs::{Envelope, EventStream, Providers, RemoteHost, SessionToken};

/// The scratch repository these journeys publish to when nothing names another.
pub const DEFAULT_SMOKE_REPO: &str = "nickderobertis/onevcs-smoke";

/// The variable that names a different scratch repository.
pub const SMOKE_REPO_ENV: &str = "ONEVCS_SMOKE_REPO";

/// What a repository's name has to end with for these journeys to touch it.
///
/// The whole guard, and deliberately a crude one: this tier pushes branches, opens
/// pull requests, and merges them, and the cost of pointing it at a repository
/// somebody works in is not recoverable. A name nobody would give a repository they
/// cared about is a rule a misconfiguration cannot slip past, where "is it the one I
/// meant?" is a judgement a program cannot make.
pub const SCRATCH_SUFFIX: &str = "-smoke";

/// The prefix every branch this tier creates carries, so what it left behind is
/// recognizable on the scratch repository.
pub const BRANCH_PREFIX: &str = "onevcs-smoke";

/// The line the scratch repository's `smoke-check.yml` writes into its job log,
/// which is what proves [`RemoteHost::check_log`] fetched the real thing rather
/// than a message about not having fetched it.
///
// llmlint: ignore[contracts_have_one_source_or_a_drift_gate] the other end of this
// agreement is a workflow file in a repository outside this one, so there is no
// checked-in source to derive it from and no gate that could run before the drift.
// The tier is the gate: the checks journey asserts the fetched log contains this
// line, so editing `smoke-check.yml` out from under it fails `just smoke-real` with
// the artifact's actual contents rather than passing quietly.
pub const CHECK_LOG_LINE: &str = "onevcs real-backend smoke: this line is what check_log fetches.";

/// The scratch repository, or a refusal naming why this is not one.
///
/// Called **before the first mutating call** in every journey: a push, a pull
/// request, and a merge are each irreversible on somebody's repository, so the
/// check cannot live any later than the first line of the test.
pub fn scratch_repo() -> String {
    let slug = std::env::var(SMOKE_REPO_ENV)
        .ok()
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| DEFAULT_SMOKE_REPO.to_owned());
    let parts: Vec<&str> = slug.split('/').collect();
    let [owner, name] = parts[..] else {
        panic!(
            "{SMOKE_REPO_ENV}={slug:?} does not name one repository as owner/name, and this tier \
             pushes branches and merges pull requests — it will not guess"
        )
    };
    assert!(
        !owner.is_empty() && !name.is_empty(),
        "{SMOKE_REPO_ENV}={slug:?} names an empty owner or repository"
    );
    assert!(
        name.ends_with(SCRATCH_SUFFIX),
        "{SMOKE_REPO_ENV}={slug:?} is not a scratch repository: this tier pushes branches, opens \
         pull requests, and merges them, so it runs only against a repository whose name ends in \
         {SCRATCH_SUFFIX:?}. Point it at one, or leave it unset for {DEFAULT_SMOKE_REPO:?}."
    );
    slug
}

/// Run `gh` as the journey's own witness, or say what is missing.
///
/// Never the implementation under test — this is how a journey asks GitHub what it
/// made of a run, the way a person would, so an assertion is never read out of the
/// same call that produced it.
pub fn gh(args: &[&str]) -> String {
    match gh_try(args) {
        Ok(out) => out,
        Err(refusal) => panic!("{refusal}"),
    }
}

/// Run `gh`, whatever it says.
pub fn gh_try(args: &[&str]) -> Result<String, String> {
    let output = Command::new("gh")
        .args(args)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|error| missing(&format!("`gh` could not be run: {error}")))?;
    if output.status.success() {
        return Ok(String::from_utf8_lossy(&output.stdout).into_owned());
    }
    Err(format!(
        "gh {} exited {}: {}",
        args.join(" "),
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stderr).trim()
    ))
}

/// `gh`'s answer as JSON, or a refusal naming the call that would not answer.
pub fn gh_json(args: &[&str]) -> Value {
    let raw = gh(args);
    serde_json::from_str(&raw)
        .unwrap_or_else(|error| panic!("gh {} printed no JSON ({error}): {raw:?}", args.join(" ")))
}

/// What a run with no credential is told, which has to be enough to fix it.
pub fn missing(what: &str) -> String {
    format!(
        "the real-backend tier needs the GitHub CLI and a credential for it, and {what}. Install \
         `gh` (https://cli.github.com), then either run `gh auth login` or set GH_TOKEN to a \
         token that can read, push to, open a pull request on, and merge in the scratch \
         repository. In CI it is the RELEASE_PLZ_TOKEN secret, passed as GH_TOKEN. This tier \
         never skips and never falls back to a fake host or a provider: a smoke that can pass \
         without talking to GitHub proves nothing, which is the whole reason it exists."
    )
}

/// The real GitHub host for the scratch repository, taken off the real providers
/// rather than named — [`onevcs::GitHub`] is reached the way a run reaches it.
pub fn real_host(slug: &str) -> Box<dyn RemoteHost> {
    Providers::real()
        .hosting
        .for_repo(slug)
        .unwrap_or_else(|error| panic!("the real hosting refused the slug {slug:?}: {error}"))
}

/// Who GitHub says is calling, asked through the interface under test.
///
/// The credential probe as well as the coverage of [`RemoteHost::authenticated_user`]:
/// this is the first host call every journey makes, so an absent or unusable
/// credential fails here, naming itself, before anything has been created.
pub fn authenticated_user(host: &dyn RemoteHost) -> String {
    match host.authenticated_user() {
        Ok(login) => login,
        Err(error) => panic!("{}", missing(&format!("the host refused: {error}"))),
    }
}

/// One scratch host: its own state root, its own git configuration, its own clone.
///
/// `HOME` is deliberately **not** redirected. `gh` reads the operator's credential
/// out of their home directory, and a world that moved it would turn "this
/// credential cannot reach the scratch repository" into "there is no credential" —
/// the one diagnosis this tier exists to report accurately.
pub struct World {
    /// Held for its lifetime: dropping it removes the scratch host.
    _directory: tempfile::TempDir,
    root: PathBuf,
    label: String,
}

impl World {
    /// A world named for the journey that runs in it.
    pub fn new(label: &str) -> Self {
        let directory = tempfile::tempdir().expect("a scratch directory");
        // Canonical, because `register` records a checkout by its real path.
        let root = std::fs::canonicalize(directory.path()).expect("a canonical scratch root");
        let world = Self {
            _directory: directory,
            root,
            label: label.to_owned(),
        };
        // Written into this world's own file rather than by `gh auth setup-git`,
        // which would edit the global configuration of whoever ran the tier.
        // `GIT_CONFIG_GLOBAL` points every git this run starts — including the ones
        // `onevcs` spawns — at this file, so the clone, the push, the fetch, and the
        // fast-forward all authenticate and nothing outside the world is configured.
        std::fs::write(
            world.gitconfig(),
            "[user]\n\tname = onevcs smoke\n\temail = smoke@example.invalid\n\
             [init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n\
             [advice]\n\tdetachedHead = false\n\
             [credential \"https://github.com\"]\n\thelper = !gh auth git-credential\n",
        )
        .expect("a git configuration");
        world
    }

    /// A path under this world's scratch root.
    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    /// This world's root, which is what the stream comparison scrubs out.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The state root every `onevcs` operation in this world shares.
    pub fn home(&self) -> PathBuf {
        self.path(".onevcs")
    }

    /// The git configuration every git this run starts reads.
    pub fn gitconfig(&self) -> PathBuf {
        self.path("gitconfig")
    }

    /// A branch name no other run of this tier can be using.
    ///
    /// Three things, and each rules out a different collision: the journey's own
    /// label separates the journeys within one run, the process id separates two
    /// runs on one host, and the scratch directory's own random name separates two
    /// runs that got the same pid on different hosts — which is what a laptop and a
    /// CI runner publishing to one scratch repository at the same moment are.
    pub fn branch(&self) -> String {
        let unique = self
            .root
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        format!(
            "{BRANCH_PREFIX}/{}-{}-{unique}",
            self.label,
            std::process::id()
        )
    }

    /// Run real git in this world, requiring it to succeed.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env("GIT_CONFIG_GLOBAL", self.gitconfig())
            .stdin(std::process::Stdio::null())
            .output()
            .expect("git must be installed");
        assert!(
            output.status.success(),
            "git {} failed in {}:\n{}{}",
            args.join(" "),
            cwd.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr),
        );
        String::from_utf8_lossy(&output.stdout).trim().to_owned()
    }

    /// Clone the scratch repository over HTTPS, which is the remote every later
    /// operation authenticates against.
    pub fn clone_scratch(&self, slug: &str) -> PathBuf {
        let checkout = self.path(CHECKOUT);
        self.git(
            &self.root,
            &[
                "clone",
                "-q",
                &format!("https://github.com/{slug}.git"),
                &checkout.to_string_lossy(),
            ],
        );
        checkout
    }

    /// A real bare origin with one commit on `main`, for the side of a comparison
    /// that is not the scratch repository.
    pub fn bare_origin(&self, name: &str) -> PathBuf {
        let seed = self.path(format!("seed-{name}"));
        std::fs::create_dir_all(&seed).expect("a seed directory");
        self.git(&seed, &["init", "-q", "-b", "main"]);
        std::fs::write(seed.join("README.md"), "# origin\n").expect("a seed file");
        self.git(&seed, &["add", "-A"]);
        self.git(&seed, &["commit", "-q", "-m", "chore: seed the repository"]);

        let origin = self.path(format!("{name}.git"));
        self.git(
            &self.root,
            &["init", "-q", "--bare", &origin.to_string_lossy()],
        );
        self.git(
            &seed,
            &["remote", "add", "origin", &origin.to_string_lossy()],
        );
        self.git(&seed, &["push", "-q", "origin", "main"]);
        std::fs::remove_dir_all(&seed).expect("the seed is disposable");
        origin
    }

    /// Clone a local origin into this world under [`CHECKOUT`].
    pub fn clone_of(&self, origin: &Path) -> PathBuf {
        let checkout = self.path(CHECKOUT);
        self.git(
            &self.root,
            &[
                "clone",
                "-q",
                &origin.to_string_lossy(),
                &checkout.to_string_lossy(),
            ],
        );
        checkout
    }

    /// Commit a file in a tree.
    pub fn commit_file(&self, checkout: &Path, file: &str, contents: &str, subject: &str) {
        std::fs::write(checkout.join(file), contents).expect("a file to commit");
        self.git(checkout, &["add", "-A"]);
        self.git(checkout, &["commit", "-q", "-m", subject]);
    }

    /// Write this world's rules file.
    pub fn rules(&self, body: &str) {
        std::fs::create_dir_all(self.home()).expect("a state root");
        std::fs::write(self.home().join("rules.yml"), body).expect("a rules file");
    }

    /// Every event a session's stream carries, read through the typed reader this
    /// crate publishes for exactly that.
    pub fn events(&self, token: &str) -> Vec<Value> {
        let session = SessionToken(token.to_owned());
        let mut stream = EventStream::open(&session)
            .unwrap_or_else(|error| panic!("the stream for {token:?} is readable: {error}"));
        stream
            .read()
            .unwrap_or_else(|error| panic!("the stream for {token:?} reads: {error}"))
            .iter()
            .map(|envelope: &Envelope| {
                serde_json::to_value(envelope).expect("an envelope serializes")
            })
            .collect()
    }

    /// One stored artifact, read out of this world's state root.
    pub fn artifact(&self, id: &str) -> String {
        let path = self.home().join("artifacts").join(id);
        std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("the artifact at {} reads: {error}", path.display()))
    }
}

/// What a session's stream recorded, by kind — the detail a failure needs to say
/// how far the run got before it stopped.
pub fn kinds_of(world: &World, token: &str) -> Vec<String> {
    world
        .events(token)
        .iter()
        .filter_map(|event| event["kind"].as_str().map(str::to_owned))
        .collect()
}

/// The directory name every world's clone of the scratch repository takes.
///
/// One name across the worlds a comparison uses, because the publication's own
/// queue identity is that path: two worlds that named their checkouts differently
/// would emit two lock identities that differ in more than their scratch root, and
/// the comparison would report a difference the backends do not have.
pub const CHECKOUT: &str = "checkout";

/// Point this process's `onevcs` at one world.
///
/// Safe as a process-wide write because `cargo nextest` runs each test in its own
/// process, and the runs a comparison makes are sequential within it. `ONEVCS_GH`
/// is deliberately never set: the program that answers as `gh` here is `gh`.
pub fn inhabit(world: &World) {
    std::env::set_var("ONEVCS_HOME", world.home());
    std::env::set_var("GIT_CONFIG_GLOBAL", world.gitconfig());
    std::env::set_var("ONEVCS_LOCK_TIMEOUT_SECONDS", "120");
}

/// The branches one run made on the scratch repository, removed however it ends.
///
/// A `Drop` rather than a line at the end of the test, because the run that most
/// needs cleaning up is the one that failed half way. The repository itself, its
/// `main`, and the pull requests this tier opened are never touched: a merged pull
/// request is the evidence the run talked to GitHub, and deleting it would remove
/// the only durable record that it did.
pub struct Scratch {
    slug: String,
    branches: BTreeSet<String>,
}

impl Scratch {
    /// A cleanup that will remove `branch` from `slug`.
    pub fn new(slug: &str, branch: &str) -> Self {
        let mut branches = BTreeSet::new();
        branches.insert(branch.to_owned());
        Self {
            slug: slug.to_owned(),
            branches,
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        for branch in &self.branches {
            // Best effort and quiet about success: a merged pull request's head
            // branch is often already gone, and a cleanup that failed must not be
            // reported as the journey failing.
            let reference = format!("repos/{}/git/refs/heads/{branch}", self.slug);
            if let Err(refusal) = gh_try(&["api", "-X", "DELETE", &reference]) {
                eprintln!("smoke: could not remove the branch {branch}: {refusal}");
            }
        }
    }
}

/// Wait for something on the real backend, reporting what it was waiting for when
/// it never arrived.
///
/// Bounded rather than unbounded, and loud rather than skipping: a GitHub Actions
/// job that never starts is a failure of this tier's premise, not a reason to pass.
pub fn until<T>(
    what: &str,
    bound: std::time::Duration,
    mut answer: impl FnMut() -> Option<T>,
) -> T {
    let started = std::time::Instant::now();
    loop {
        if let Some(value) = answer() {
            return value;
        }
        assert!(
            started.elapsed() < bound,
            "waited {}s for {what} and it never happened",
            bound.as_secs()
        );
        std::thread::sleep(std::time::Duration::from_secs(POLL_SECONDS));
    }
}

/// How often the real backend is asked again. Seconds, because every question is
/// an API call against a rate limit.
const POLL_SECONDS: u64 = 5;
