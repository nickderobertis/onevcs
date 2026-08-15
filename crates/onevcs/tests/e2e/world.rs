//! The fixtures the lifecycle journeys are driven against.
//!
//! Everything here is **real** except the remote host's decisioning. Origins are
//! real bare repositories, checkouts are real clones, hooks are real executable
//! files git runs, and every publication is a real `git push` into a real origin.
//! What is substituted is the `gh` program: it answers as GitHub would about which
//! change requests exist and what their checks say — and when it merges one, it
//! does so with real git against the real bare origin.
//!
//! Unix only. The substituted host and the hooks the gate journeys install are
//! POSIX shell, which is what the repositories this tool drives actually carry.

// llmlint: ignore-file[e2e_not_mocked] the one boundary an offline gate cannot drive
// is the remote host's own decisioning — which change requests exist, what their
// checks say, whether a merge is allowed. That is what the program installed here as
// `gh` answers, and nothing else is substituted: origins are real bare repositories,
// checkouts are real clones, hooks are real files git runs, every publication is a
// real `git push`, and when this program merges a change it does so with real git
// against the same bare origin. A journey asserting that a change reached its base
// is therefore asserting about git, not about this fixture.

#![cfg(unix)]

use std::collections::BTreeSet;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

use assert_cmd::cargo::CommandCargoExt;
use fs4::fs_std::FileExt;

/// One scratch host: its own home, its own `onevcs` state root, its own origins.
pub struct World {
    /// Held for its lifetime: dropping it removes the scratch host.
    _directory: tempfile::TempDir,
    root: PathBuf,
}

impl World {
    /// A host with git configured and an empty `onevcs` state root.
    pub fn new() -> Self {
        let directory = tempfile::tempdir().expect("a scratch directory");
        // Canonical, because `register` records a checkout by its real path and a
        // path rule is matched against that. On a host whose temporary directory is
        // reached through a symlink — macOS's `/var` is one — a journey built on the
        // uncanonical name would write a rule that silently matches nothing, which
        // is the fixture disagreeing with the tool rather than a finding.
        let root = std::fs::canonicalize(directory.path()).expect("a canonical scratch root");
        let world = Self {
            _directory: directory,
            root,
        };
        std::fs::write(
            world.path(".gitconfig"),
            "[user]\n\tname = Journey\n\temail = journey@example.invalid\n\
             [init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n\
             [advice]\n\tdetachedHead = false\n",
        )
        .expect("a git configuration");
        world
    }

    /// A path under this host's scratch root.
    pub fn path(&self, relative: impl AsRef<Path>) -> PathBuf {
        self.root.join(relative)
    }

    /// The state root every `onevcs` invocation in this world shares.
    pub fn home(&self) -> PathBuf {
        self.path(".onevcs")
    }

    /// The `onevcs` binary, pointed at this world.
    pub fn onevcs(&self) -> assert_cmd::Command {
        let mut command =
            std::process::Command::cargo_bin("onevcs").expect("the binary must be built");
        command
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .env("ONEVCS_HOME", self.home())
            // Journeys must not wait out a production bound when something is
            // genuinely stuck; each one that tests a bound sets its own.
            .env("ONEVCS_LOCK_TIMEOUT_SECONDS", "60")
            .env("ONEVCS_CHECKS_POLL_SECONDS", "0.02")
            .env("ONEVCS_CHECKS_TIMEOUT_SECONDS", "20")
            .env("ONEVCS_GH", self.path("bin/gh"))
            .env("ONEVCS_FAKE_GH_STATE", self.path("gh-state"))
            .current_dir(&self.root);
        // The one inherited variable: a coverage run tells the instrumented binary
        // where to write its profile. Cleared, it falls back to the working
        // directory — which for the commands that run inside a checkout is a stray
        // file in a tree these journeys assert is clean.
        if let Some(profile) = std::env::var_os("LLVM_PROFILE_FILE") {
            command.env("LLVM_PROFILE_FILE", profile);
        }
        assert_cmd::Command::from_std(command)
    }

    /// Every advisory lock file this world's state root holds so far.
    ///
    /// A lock is named after a digest of what it guards, so which one guards a
    /// given run root is read off *when it appears* rather than recomputed here.
    pub fn locks(&self) -> BTreeSet<PathBuf> {
        std::fs::read_dir(self.home().join("locks"))
            .into_iter()
            .flatten()
            .flatten()
            .map(|entry| entry.path())
            .collect()
    }

    /// Hold one of this world's advisory locks exclusively, exactly as a second
    /// `onevcs` working inside that run root does. Released when the file is dropped.
    pub fn occupy(lock: &Path) -> std::fs::File {
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(lock)
            .unwrap_or_else(|e| panic!("the lock at {} is openable: {e}", lock.display()));
        assert!(
            FileExt::try_lock_exclusive(&file).expect("the lock is takeable"),
            "nothing else may hold {} when a journey occupies it",
            lock.display()
        );
        file
    }

    /// Run real git, requiring it to succeed.
    pub fn git(&self, cwd: &Path, args: &[&str]) -> String {
        let output = self.git_raw(cwd, args);
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

    /// Run real git, whatever it says.
    pub fn git_raw(&self, cwd: &Path, args: &[&str]) -> std::process::Output {
        Command::new("git")
            .args(args)
            .current_dir(cwd)
            .env_clear()
            .env("PATH", std::env::var("PATH").unwrap_or_default())
            .env("HOME", &self.root)
            .output()
            .expect("git must be installed")
    }

    /// A real bare origin with one commit on `main`, and its clone URL.
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
        // A non-bare receiver would refuse the publication push; a bare one is what
        // an origin is.
        origin
    }

    /// Clone an origin into this world and return the checkout.
    pub fn clone_of(&self, origin: &Path, name: &str) -> PathBuf {
        let checkout = self.path(name);
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

    /// Commit a file on a branch of a checkout.
    pub fn commit_file(&self, checkout: &Path, file: &str, contents: &str, subject: &str) {
        std::fs::write(checkout.join(file), contents).expect("a file to commit");
        self.git(checkout, &["add", "-A"]);
        self.git(checkout, &["commit", "-q", "-m", subject]);
    }

    /// Install an executable `pre-push` hook running `body`.
    pub fn install_pre_push(&self, checkout: &Path, body: &str) -> PathBuf {
        let hooks = self.path(format!(
            "hooks-{}",
            checkout.file_name().unwrap_or_default().to_string_lossy()
        ));
        std::fs::create_dir_all(&hooks).expect("a hooks directory");
        let hook = hooks.join("pre-push");
        write_script(
            &hook,
            &format!("#!/usr/bin/env bash\nset -euo pipefail\n{body}\n"),
        );
        self.git(
            checkout,
            &["config", "core.hooksPath", &hooks.to_string_lossy()],
        );
        hook
    }

    /// Install the program that answers as `gh` for one origin.
    pub fn install_fake_host(&self, origin: &Path) {
        let bin = self.path("bin");
        std::fs::create_dir_all(&bin).expect("a bin directory");
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(
            self.path("gh-state/origin"),
            origin.to_string_lossy().as_bytes(),
        )
        .expect("the host must know which origin it merges into");
        write_script(&bin.join("gh"), FAKE_GH);
    }

    /// What the substituted host reports as a change request's checks.
    ///
    /// One `|`-separated row per check: name, status, conclusion, and whether it
    /// is required. Not a tab — bash's `read` collapses runs of IFS *whitespace*
    /// however IFS is set, which silently eats a check with no conclusion yet.
    /// The host renders its rollup from this and decides whether a merge may
    /// proceed from the same rows, so what it reports and what it acts on cannot
    /// disagree.
    pub fn host_checks(&self, checks: &[Check]) {
        let rows: String = checks
            .iter()
            .map(|check| {
                format!(
                    "{}|{}|{}|{}\n",
                    check.name,
                    check.status,
                    check.conclusion.unwrap_or(""),
                    if check.required { "true" } else { "false" }
                )
            })
            .collect();
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path("gh-state/checks.rows"), rows).expect("a check rollup");
    }

    /// Make the substituted host answer in a shape it has no business answering in.
    ///
    /// `no-head` drops the commit a change request's checks are reported against,
    /// `no-number` drops its identifier, `rollup-not-a-list` answers about its
    /// checks with something that is not a list of them, `no-state` will not say
    /// whether it is open or merged, and `no-url` / `url-names-no-change` print
    /// something other than a change request's URL when one is opened.
    ///
    /// Four are about the second call a check's log takes — where the job that ran
    /// it is: `no-check-list` refuses to list the checks at all, `no-job` reports
    /// the check in the rollup and then lists no job for it, `jobless-link` names a
    /// details URL that is not a job's, and `non-list` answers with JSON that is not
    /// a list of checks at all.
    ///
    /// `checks-refused` is a credential that can read no check source at all, and
    /// the `actions-only` family is the one this crate's real tier runs under: a
    /// fine-grained token, which GitHub will not let resolve a check run under any
    /// permission and which therefore reads the Actions API and the repository's
    /// rulesets or nothing. `actions-only-truncated` has the Actions listing hold
    /// entries back, `actions-only-rules-not-a-list` answers about the rulesets with
    /// something that is not a list of them, and `actions-only-rules-unsaid` names a
    /// ruleset that requires status checks and will not say which.
    pub fn answer_malformed(&self, shape: &str) {
        std::fs::write(self.path("gh-state/malformed"), shape)
            .expect("a host that answers in the wrong shape");
    }

    /// Make the substituted host refuse to say which of its checks block the merge.
    ///
    /// It answers about the checks themselves as usual and then declines the one
    /// question that decides whether a merge was gated — which is the call `gh` puts
    /// it behind, `pr checks --required`. Deliberately not the wording a repository
    /// that requires nothing gets: "none block" is an answer, and this is a refusal
    /// to answer.
    pub fn report_checks_that_do_not_say_if_they_block(&self) {
        std::fs::write(self.path("gh-state/partial-checks"), "")
            .expect("a host that answers partially");
    }

    /// Make the substituted host accept a merge and then not perform it.
    pub fn accept_merges_without_performing_them(&self) {
        std::fs::write(self.path("gh-state/refuse-merge"), "")
            .expect("a host that says yes and does nothing");
    }

    /// Make the substituted host unable to hand over a check's log.
    pub fn refuse_check_logs(&self) {
        std::fs::write(self.path("gh-state/no-logs"), "").expect("a host that keeps its logs");
    }

    /// What the job behind one check printed, which is what its log is.
    pub fn host_log(&self, check: &str, log: &str) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path(format!("gh-state/log-{check}.txt")), log).expect("a check log");
    }

    /// Make the substituted host guard its output the way a current `gh` does: a
    /// log carrying terminal escape sequences is refused unless the call asked for
    /// them.
    pub fn guard_terminal_escapes(&self) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path("gh-state/guards-escapes"), "")
            .expect("a host that guards its output");
    }

    /// Make the substituted host a `gh` from before that flag existed, which
    /// rejects it outright — the generation a workstation still has.
    pub fn reject_the_escape_flag(&self) {
        std::fs::create_dir_all(self.path("gh-state")).expect("a host state directory");
        std::fs::write(self.path("gh-state/no-escape-flag"), "")
            .expect("a host that has not heard of the flag");
    }

    /// Every call the substituted host has been asked to make, in order.
    ///
    /// What a journey about a credential's *reach* asserts over. Whether a build
    /// can answer under a token that may not resolve a check run is decided by
    /// which endpoints it asks for, and an answer cannot show that: this world
    /// replies to calls the real host would refuse.
    pub fn host_calls(&self) -> Vec<String> {
        std::fs::read_to_string(self.path("gh-state/gh-calls.log"))
            .unwrap_or_default()
            .lines()
            .map(str::trim)
            .filter(|call| !call.is_empty())
            .map(str::to_owned)
            .collect()
    }

    /// Every event a session's stream carries, read the way a consumer reads it.
    pub fn events(&self, token: &str) -> Vec<serde_json::Value> {
        let output = self
            .onevcs()
            .args(["events", token])
            .output()
            .expect("the binary runs");
        assert!(
            output.status.success(),
            "`onevcs events {token}` failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8_lossy(&output.stdout)
            .lines()
            .filter(|line| !line.trim().is_empty())
            .map(|line| serde_json::from_str(line).expect("every event is one JSON object"))
            .collect()
    }

    /// The events of one kind, in order.
    pub fn events_of(&self, token: &str, kind: &str) -> Vec<serde_json::Value> {
        self.events(token)
            .into_iter()
            .filter(|event| event["kind"] == kind)
            .collect()
    }
}

fn write_script(path: &Path, contents: &str) {
    std::fs::write(path, contents).expect("a script");
    let mut permissions = std::fs::metadata(path)
        .expect("a written script")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(path, permissions).expect("an executable script");
}

/// One check the substituted host reports.
pub struct Check {
    /// The check's name, as branch protection lists it.
    pub name: &'static str,
    /// Where it is: `completed`, or anything else for still running.
    pub status: &'static str,
    /// How it ended, once it has.
    pub conclusion: Option<&'static str>,
    /// Whether it blocks the merge.
    pub required: bool,
}

/// The token printed by `onevcs session open`.
pub fn token_of(stdout: &[u8]) -> String {
    let value: serde_json::Value =
        serde_json::from_slice(stdout).expect("session open prints one JSON object");
    value["token"]
        .as_str()
        .expect("a session carries a token")
        .to_owned()
}

/// The worktree printed by `onevcs session open`.
pub fn worktree_of(stdout: &[u8]) -> PathBuf {
    let value: serde_json::Value =
        serde_json::from_slice(stdout).expect("session open prints one JSON object");
    PathBuf::from(
        value["worktree"]
            .as_str()
            .expect("a session carries a worktree"),
    )
}

/// GitHub's decisioning, and nothing else.
///
/// It records which change requests exist and what their checks say. When it is
/// asked to merge one it performs the merge **with real git against the real bare
/// origin** — so a journey that asserts a change reached its base is asserting
/// about git, not about this script.
const FAKE_GH: &str = r##"#!/usr/bin/env bash
set -euo pipefail

STATE="${ONEVCS_FAKE_GH_STATE:?the substituted host needs a state directory}"
mkdir -p "$STATE"

# Every call this host is asked to make, one line each, in order. A journey about
# what a *credential* can reach has to assert over this rather than over the
# answer: a stand-in that replies is indistinguishable from one that was allowed
# to be asked, so which endpoints a path touched is readable nowhere else. An
# argument's own newlines are folded into spaces so one call stays one line.
{ printf '%s ' "$@" | tr '\n' ' '; printf '\n'; } >>"$STATE/gh-calls.log"

# Whether this call asked to be shown content carrying terminal escape sequences,
# and whether this world's `gh` predates the flag. One that does rejects it here,
# while parsing its arguments, rather than after asking GitHub anything.
allow_escapes=0
for argument in "$@"; do
  [ "$argument" = "--allow-escape-sequences" ] || continue
  allow_escapes=1
done
if [ "$allow_escapes" = "1" ] && [ -f "$STATE/no-escape-flag" ]; then
  printf 'unknown flag: --allow-escape-sequences\n' >&2
  exit 1
fi

ORIGIN="$(cat "$STATE/origin")"
CHECKS="$STATE/checks.rows"
malformed="$(cat "$STATE/malformed" 2>/dev/null || printf '')"

# What a credential GitHub will not let read a change request's check runs is told,
# in the two shapes it is told it: the GraphQL rollup names the node it would not
# produce, and the REST endpoints answer 403. Both are refusals, and neither says
# there are no checks.
refused_graphql() {
  printf 'GraphQL: Resource not accessible by personal access token (repository.pullRequest.statusCheckRollup.nodes.0.commit.statusCheckRollup.contexts.nodes.0)\n' >&2
  exit 1
}

# Whether this world's credential may read check runs at all. `actions-only` is the
# fine-grained token the real tier runs under: it reads the Actions API and the
# repository's rulesets, and nothing that resolves a check run.
readable_rollup() {
  case "$malformed" in
    checks-refused|actions-only*|misleading-refusal) return 1 ;;
    *) return 0 ;;
  esac
}

# One job's log, addressed by the job's **id** — the id a row has in this world,
# which is what both `gh run view --job` and the Actions API answer to.
#
# The only answer here that is content rather than this host's own words, so the
# only one the escape guard can fire on. It fires on what the content carries, the
# way `gh` does, rather than on having been asked.
job_log() {
  local want="$1" name source
  if [ -f "$STATE/no-logs" ]; then
    printf 'this repository keeps its check logs to itself\n' >&2
    exit 1
  fi
  name="$(awk -F'|' -v want="$want" 'NF && ++row == want { print $1 }' "$CHECKS" 2>/dev/null || printf '')"
  if [ -z "$name" ]; then
    printf 'could not find any jobs with ID %s\n' "$want" >&2
    exit 1
  fi
  source="$STATE/log-$name.txt"
  if [ ! -f "$source" ]; then
    source="$STATE/rendered-log"
    printf 'the host log for check %s\n' "$name" >"$source"
  fi
  if [ -f "$STATE/guards-escapes" ] && [ "$allow_escapes" = "0" ] \
    && LC_ALL=C grep -q "$(printf '\033')" "$source"; then
    printf 'the response contains terminal escape sequences; pass --allow-escape-sequences to output it anyway\n' >&2
    exit 1
  fi
  cat "$source"
}

command="${1:-}"; shift || true

case "$command" in
  api)
    path="${1:-}"; shift || true
    query=""
    case "$path" in
      *\?*) query="${path#*\?}"; path="${path%%\?*}" ;;
    esac
    if [ "$path" = "user" ]; then
      printf 'tester\n'
      exit 0
    fi
    # Everything below is GitHub's REST API: the Actions endpoints and the
    # repository's rulesets, which is what a token that may not resolve a check run
    # is left with. `checks-refused` is the credential that may not read those
    # either, and it declines here the way the API declines.
    if [ "$malformed" = "checks-refused" ]; then
      printf 'gh: Resource not accessible by personal access token (HTTP 403)\n' >&2
      exit 1
    fi
    case "$path" in
      */actions/runs)
        head_sha="${query#*head_sha=}"; head_sha="${head_sha%%&*}"
        runs=0
        for record in "$STATE"/pr-*.env; do
          [ -e "$record" ] || continue
          if ( . "$record"; [ "$PR_HEAD_SHA" = "$head_sha" ] ); then runs=1; fi
        done
        if [ "$runs" = "0" ]; then
          # No workflow has run on that commit, which is what a commit nothing was
          # opened against reports.
          printf '{"total_count":0,"workflow_runs":[]}\n'
          exit 0
        fi
        printf '{"total_count":1,"workflow_runs":[{"id":1,"head_sha":"%s","status":"completed"}]}\n' "$head_sha"
        exit 0 ;;
      */actions/runs/*/jobs)
        # The same rows the rollup is rendered from, as the jobs of that one run —
        # so what this host reports through either source and what it acts on when
        # asked to merge cannot disagree. A job's id is its row, which is the id
        # `gh run view --job` answers to as well.
        rows=""; separator=""; row=0; entry=""
        while IFS='|' read -r name status conclusion required; do
          [ -n "$name" ] || continue
          row=$((row + 1))
          if [ -n "$conclusion" ]; then entry="\"$conclusion\""; else entry=null; fi
          rows="$rows$separator{\"id\":$row,\"name\":\"$name\",\"status\":\"$status\",\"conclusion\":$entry}"
          separator=","
        done <"$CHECKS" 2>/dev/null || true
        total="$row"
        # A page that held entries back. The listing says how many there are, and a
        # build that read the short list as the whole answer would wait for a check
        # it was never shown.
        if [ "$malformed" = "actions-only-truncated" ]; then total=$((row + 1)); fi
        printf '{"total_count":%s,"jobs":[%s]}\n' "$total" "$rows"
        exit 0 ;;
      */actions/jobs/*/logs)
        job="${path%/logs}"; job="${job##*/}"
        job_log "$job"
        exit 0 ;;
      */rules/branches/*)
        case "$malformed" in
          actions-only-rules-not-a-list)
            printf '{"rules":[]}\n'
            exit 0 ;;
          actions-only-rules-unsaid)
            # A ruleset that says it requires status checks and will not say which.
            printf '[{"type":"required_status_checks","parameters":{}}]\n'
            exit 0 ;;
        esac
        contexts=""; separator=""
        while IFS='|' read -r name status conclusion required; do
          [ -n "$name" ] || continue
          [ "$required" = "true" ] || continue
          contexts="$contexts$separator{\"context\":\"$name\",\"integration_id\":15368}"
          separator=","
        done <"$CHECKS" 2>/dev/null || true
        if [ -z "$contexts" ]; then
          # A repository with no ruleset requiring anything, which is what the
          # scratch repository the real tier runs against answers.
          printf '[]\n'
          exit 0
        fi
        printf '[{"type":"required_status_checks","ruleset_source_type":"Repository","parameters":{"strict_required_status_checks_policy":false,"required_status_checks":[%s]}}]\n' "$contexts"
        exit 0 ;;
    esac
    printf 'fake gh: unsupported api path %s\n' "$path" >&2
    exit 1
    ;;
  run)
    # `gh run view --log --job` addresses a job by its **id**, never by a check's
    # name, so this answers to the id `pr checks` reported for that row and to
    # nothing else — a caller that passed a name would fail here as it does against
    # the real host.
    job=""
    while [ $# -gt 0 ]; do
      case "$1" in
        --job) job="${2:-}"; shift 2 ;;
        *) shift ;;
      esac
    done
    job_log "$job"
    exit 0
    ;;
  pr) ;;
  *)
    printf 'fake gh: unsupported command %s\n' "$command" >&2
    exit 1
    ;;
esac

subcommand="${1:-}"; shift || true
number=""
case "$subcommand" in
  view|merge|checks) number="${1:-}"; shift || true ;;
esac

repo=""; head=""; base=""; title=""; body=""; auto=0; json_fields=""; only_required=0
while [ $# -gt 0 ]; do
  case "$1" in
    --repo) repo="${2:-}"; shift 2 ;;
    --head) head="${2:-}"; shift 2 ;;
    --base) base="${2:-}"; shift 2 ;;
    --title) title="${2:-}"; shift 2 ;;
    --body) body="${2:-}"; shift 2 ;;
    --json) json_fields="${2:-}"; shift 2 ;;
    --state) shift 2 ;;
    --auto) auto=1; shift ;;
    --required) only_required=1; shift ;;
    *) shift ;;
  esac
done

# The rollup and the merge decision are rendered from the same rows, so what the
# host reports and what it acts on cannot disagree.
#
# It carries no `isRequired`, because `gh pr view` carries none: its rollup says
# what a check is called, where it is, and how it ended, and nothing about whether
# it blocks anything. Emitting one here is what let this crate read a field the
# real host has never returned. Whether a check blocks is answered by
# `pr checks --required`, below.
rollup() {
  printf '['
  local separator="" name status conclusion required entry
  while IFS='|' read -r name status conclusion required; do
    [ -n "$name" ] || continue
    if [ -n "$conclusion" ]; then entry="\"$conclusion\""; else entry=null; fi
    printf '%s{"__typename":"CheckRun","name":"%s","status":"%s","conclusion":%s}' \
      "$separator" "$name" "$status" "$entry"
    separator=","
  done <"$CHECKS" 2>/dev/null || true
  printf ']'
}

verdict() {
  local name status conclusion required total settled red
  total=0; settled=0; red=0
  while IFS='|' read -r name status conclusion required; do
    [ -n "$name" ] || continue
    [ "$required" = "true" ] || continue
    total=$((total + 1))
    [ "$status" = "completed" ] || continue
    settled=$((settled + 1))
    case "$conclusion" in
      success|skipped|neutral) ;;
      *) red=1 ;;
    esac
  done <"$CHECKS" 2>/dev/null || true
  if [ "$red" = "1" ]; then printf 'red'
  elif [ "$total" -gt 0 ] && [ "$total" = "$settled" ]; then printf 'green'
  else printf 'pending'; fi
}

case "$subcommand" in
  checks)
    # Where `gh` reports whether a check blocks the merge, and where each check
    # ran. `gh pr view`'s rollup says neither, which is why this is a second call.
    . "$STATE/pr-$number.env"
    # It resolves the same check runs the rollup does, so a credential refused
    # there is refused here — this command is not a second way in.
    readable_rollup || refused_graphql
    if [ "$only_required" = "0" ]; then
      case "$malformed" in
        no-check-list)
          printf 'the host will not list the checks on this change request\n' >&2
          exit 1 ;;
        no-job)
          # It reported the check and will not say where it ran.
          printf '[]\n'
          exit 0 ;;
        jobless-link)
          printf '[{"name":"%s","state":"COMPLETED","link":"https://github.com/%s/actions/runs/1"}]\n' \
            "$(awk -F'|' 'NF { print $1; exit }' "$CHECKS")" "$repo"
          exit 0 ;;
        non-list)
          # Well-formed JSON that is not a list of checks: an answer nothing can be
          # searched for a job, as distinct from one that lists no job.
          printf '{"checks":[]}\n'
          exit 0 ;;
      esac
    fi
    if [ "$only_required" = "1" ] && [ -f "$STATE/partial-checks" ]; then
      # A host that will not say which of its checks block the merge. Deliberately
      # not the "no required checks" wording, which means the opposite: that the
      # repository requires none and the host knows it.
      printf 'the host declines to say which of its checks block the merge\n' >&2
      exit 1
    fi
    rows=""; separator=""; row=0; unsettled=0
    while IFS='|' read -r name status conclusion required; do
      [ -n "$name" ] || continue
      # Counted over every row, filtered or not, so the job id a row reports is the
      # one `gh run view --job` answers to.
      row=$((row + 1))
      if [ "$only_required" = "1" ] && [ "$required" != "true" ]; then continue; fi
      [ "$status" = "completed" ] || unsettled=1
      rows="$rows$separator{\"name\":\"$name\",\"state\":\"$status\",\"link\":\"https://github.com/$repo/actions/runs/1/job/$row\"}"
      separator=","
    done <"$CHECKS" 2>/dev/null || true
    if [ "$only_required" = "1" ] && [ -z "$rows" ]; then
      printf "no required checks reported on the '%s' branch\n" "$PR_HEAD" >&2
      exit 1
    fi
    printf '[%s]\n' "$rows"
    # gh reports a non-zero status when a check it has just printed has not
    # settled. The rollup above is still the answer, and a caller that read this
    # as a failure would be unable to watch a check at all.
    [ "$unsettled" = "0" ] || exit 8
    exit 0
    ;;
  list)
    printf '['
    separator=""
    for record in "$STATE"/pr-*.env; do
      [ -e "$record" ] || continue
      . "$record"
      [ "$PR_STATE" = "OPEN" ] || continue
      [ "$PR_HEAD" = "$head" ] || continue
      [ "$PR_BASE" = "$base" ] || continue
      case "$malformed" in
        no-number)
          printf '%s{"url":"%s","state":"%s","headRefOid":"%s"}' \
            "$separator" "$PR_URL" "$PR_STATE" "$PR_HEAD_SHA"
          separator=","
          continue ;;
        no-head)
          printf '%s{"number":%s,"url":"%s","state":"%s"}' \
            "$separator" "$PR_NUMBER" "$PR_URL" "$PR_STATE"
          separator=","
          continue ;;
      esac
      printf '%s{"number":%s,"url":"%s","state":"%s","headRefOid":"%s"}' \
        "$separator" "$PR_NUMBER" "$PR_URL" "$PR_STATE" "$PR_HEAD_SHA"
      separator=","
    done
    printf ']\n'
    ;;
  create)
    case "$malformed" in
      no-url)
        printf 'created something, somewhere\n'
        exit 0 ;;
      url-names-no-change)
        printf 'https://github.com/%s/pulls\n' "$repo"
        exit 0 ;;
    esac
    next=1
    while [ -f "$STATE/pr-$next.env" ]; do next=$((next + 1)); done
    head_sha="$(git --git-dir "$ORIGIN" rev-parse "refs/heads/$head" 2>/dev/null || printf 'unknown')"
    {
      printf 'PR_NUMBER=%s\n' "$next"
      printf 'PR_URL=https://github.com/%s/pull/%s\n' "$repo" "$next"
      printf 'PR_STATE=OPEN\n'
      printf 'PR_HEAD=%s\n' "$head"
      printf 'PR_BASE=%s\n' "$base"
      printf 'PR_HEAD_SHA=%s\n' "$head_sha"
      printf 'PR_MERGE_COMMIT=\n'
    } >"$STATE/pr-$next.env"
    printf '%s\n' "$title" >"$STATE/pr-$next.title"
    printf '%s\n' "$body" >"$STATE/pr-$next.body"
    printf 'https://github.com/%s/pull/%s\n' "$repo" "$next"
    ;;
  view)
    . "$STATE/pr-$number.env"
    # `gh` returns exactly the fields it was asked for, and so does this: a caller
    # that reads a field out of an answer it never requested is a caller that works
    # here and fails against the real host.
    wanted() { case ",$json_fields," in *",$1,"*) return 0 ;; *) return 1 ;; esac; }
    merge_commit=null
    if [ -n "$PR_MERGE_COMMIT" ]; then merge_commit="{\"oid\":\"$PR_MERGE_COMMIT\"}"; fi
    case "$malformed" in
      no-head)
        printf '{"number":%s,"state":"%s","mergeCommit":null,"statusCheckRollup":[]}\n' \
          "$PR_NUMBER" "$PR_STATE"
        exit 0 ;;
      rollup-not-a-list)
        printf '{"number":%s,"state":"%s","headRefOid":"%s","mergeCommit":null,"statusCheckRollup":"soon"}\n' \
          "$PR_NUMBER" "$PR_STATE" "$PR_HEAD_SHA"
        exit 0 ;;
      no-state)
        # Its checks are answered as usual, so the publication reaches the merge —
        # which is the call that has to know whether the change is already merged.
        printf '{"number":%s,"headRefOid":"%s","mergeCommit":null,"statusCheckRollup":%s}\n' \
          "$PR_NUMBER" "$PR_HEAD_SHA" "$(rollup)"
        exit 0 ;;
      misleading-refusal)
        if wanted statusCheckRollup; then
          printf 'GraphQL: another field said Resource not accessible while the checks service was unavailable\n' >&2
          exit 1
        fi
        ;;
      checks-refused|actions-only*)
        # What real GitHub answers a credential the repository does not allow to read
        # its checks: `gh pr view` fails *whole*, however much of what it was asked
        # for the token could see. Every other field list is answered as usual, which
        # is the point — a caller that asked only for what it reads is unaffected.
        if wanted statusCheckRollup; then
          refused_graphql
        fi
        ;;
    esac
    separator=""
    printf '{'
    if wanted number; then printf '%s"number":%s' "$separator" "$PR_NUMBER"; separator=","; fi
    if wanted state; then printf '%s"state":"%s"' "$separator" "$PR_STATE"; separator=","; fi
    if wanted mergeStateStatus; then printf '%s"mergeStateStatus":"CLEAN"' "$separator"; separator=","; fi
    if wanted headRefOid; then printf '%s"headRefOid":"%s"' "$separator" "$PR_HEAD_SHA"; separator=","; fi
    if wanted mergeCommit; then printf '%s"mergeCommit":%s' "$separator" "$merge_commit"; separator=","; fi
    if wanted statusCheckRollup; then printf '%s"statusCheckRollup":%s' "$separator" "$(rollup)"; fi
    printf '}\n'
    ;;
  merge)
    . "$STATE/pr-$number.env"
    if [ "$auto" = "1" ] && [ -f "$STATE/auto-merge-unavailable" ]; then
      printf 'Auto-merge is not enabled for this repository\n' >&2
      exit 1
    fi
    if [ "$auto" = "1" ] && [ "$(verdict)" != "green" ]; then
      # Native auto-merge: the host holds the change and lands it when its own
      # required checks pass. Nothing merges now.
      exit 0
    fi
    if [ -f "$STATE/refuse-merge" ]; then
      # Accepted, and then nothing happens — the shape a caller cannot tell from a
      # merge that worked without asking the host again.
      exit 0
    fi
    work="$STATE/merge-$PR_NUMBER"
    rm -rf "$work"
    git clone -q "$ORIGIN" "$work"
    git -C "$work" checkout -q "$PR_BASE"
    git -C "$work" merge -q --squash "origin/$PR_HEAD"
    git -C "$work" commit -q -m "$(cat "$STATE/pr-$PR_NUMBER.title") (#$PR_NUMBER)"
    git -C "$work" push -q origin "$PR_BASE"
    oid="$(git -C "$work" rev-parse HEAD)"
    {
      printf 'PR_NUMBER=%s\n' "$PR_NUMBER"
      printf 'PR_URL=%s\n' "$PR_URL"
      printf 'PR_STATE=MERGED\n'
      printf 'PR_HEAD=%s\n' "$PR_HEAD"
      printf 'PR_BASE=%s\n' "$PR_BASE"
      printf 'PR_HEAD_SHA=%s\n' "$PR_HEAD_SHA"
      printf 'PR_MERGE_COMMIT=%s\n' "$oid"
    } >"$STATE/pr-$PR_NUMBER.env"
    rm -rf "$work"
    ;;
  *)
    printf 'fake gh: unsupported pr subcommand %s\n' "$subcommand" >&2
    exit 1
    ;;
esac
"##;
