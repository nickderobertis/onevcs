//! Driving GitHub through the `gh` CLI.
//!
//! This is the one boundary an offline gate cannot exercise for free, so it is kept
//! narrow and behind one seam: every call goes through [`invoke`] or [`attempt`],
//! and the program either of them runs is `ONEVCS_GH` when that names one. An
//! offline journey therefore fakes GitHub's *decisioning* — which change requests
//! exist, what its checks say, whether a merge is allowed — while the merge itself
//! is performed with real git against a real origin. Nothing about the repository
//! side is simulated.
//!
//! `ONEVCS_GH` is what `tests/smoke/` deliberately never sets: there the program
//! that answers as `gh` is `gh`, which is the only way this module's own reading of
//! what `gh` prints can be wrong in a way a test can see.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::error::{self, Error, Result};

/// Names the program that stands in for `gh`.
pub const PROGRAM_ENV: &str = "ONEVCS_GH";
/// Selects the check-reading strategy, when an operator narrows it. The Actions
/// strategy also reads branch rules; unset, the complete rollup is tried first.
pub const CHECK_SOURCE_ENV: &str = "ONEVCS_CHECK_SOURCE";
/// How long a wait for the host's checks may last.
pub const CHECKS_TIMEOUT_ENV: &str = "ONEVCS_CHECKS_TIMEOUT_SECONDS";
/// How often the host is asked again while its checks are unsettled.
pub const CHECKS_POLL_ENV: &str = "ONEVCS_CHECKS_POLL_SECONDS";
/// The default bound on waiting for required checks. Long, because a repository's
/// CI is doing the work and abandoning it mid-flight leaves state nobody recorded.
pub const DEFAULT_CHECKS_TIMEOUT_SECONDS: f64 = 3600.0;
/// The default interval between asking the host again.
///
/// Thirty seconds against an hour-long bound. Each ask is a `gh` subprocess and at
/// least one API call, so a five-second interval spent about three hundred and
/// sixty of them watching a half-hour CI run — for an answer that changes when a
/// job finishes, not when it is asked about. `ONEVCS_CHECKS_POLL_SECONDS` still
/// overrides it, which is how a journey proves a bound rather than waiting one out.
// llmlint: ignore[changed_behavior_has_e2e] observing this default costs thirty seconds
// of sleep; tests/contract.rs holds the number to the contract that publishes it.
pub const DEFAULT_CHECKS_POLL_SECONDS: f64 = 30.0;

/// The program that answers as `gh`.
pub fn program() -> PathBuf {
    std::env::var_os(PROGRAM_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("gh"))
}

/// What one `gh` invocation wrote, and how it ended.
#[derive(Debug, Clone)]
pub struct Answer {
    /// The status it exited with, or `None` where a signal ended it — the shape
    /// [`std::process::ExitStatus::code`] answers in, kept rather than flattened
    /// into a status no process ever exits with.
    pub code: Option<i32>,
    /// Standard output.
    pub stdout: String,
    /// Standard error.
    pub stderr: String,
}

impl Answer {
    /// The most specific thing it wrote, for a message a human reads.
    pub fn detail(&self) -> String {
        if self.stderr.trim().is_empty() {
            self.stdout.trim().to_owned()
        } else {
            self.stderr.trim().to_owned()
        }
    }
}

/// Run one `gh` invocation and return what it wrote, whatever its status.
///
/// [`invoke`] is this with a non-zero status turned into a refusal, which is right
/// for every call whose answer is only the answer. `gh pr checks` is the exception
/// this exists for: it prints its rollup *and* reports a non-zero status while a
/// check it just reported has not settled, and reading that as a failure would make
/// every unsettled check unreadable.
pub fn attempt(args: &[&str]) -> Result<Answer> {
    let output = Command::new(program())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(error::at("run", &program()))?;
    Ok(Answer {
        code: output.status.code(),
        stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
        stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
    })
}

/// Run one `gh` invocation and return its standard output.
pub fn invoke(args: &[&str]) -> Result<String> {
    let answer = attempt(args)?;
    if answer.code == Some(0) {
        return Ok(answer.stdout);
    }
    Err(refused(args, &answer))
}

/// What a `gh` call that did not succeed is reported as, naming what was run.
fn refused(args: &[&str], answer: &Answer) -> Error {
    Error::Invalid {
        reason: format!("gh {} failed: {}", args.join(" "), answer.detail()),
    }
}

/// What a `gh` that guards its output takes to print content carrying terminal
/// escape sequences anyway.
const ALLOW_ESCAPES: &str = "--allow-escape-sequences";

/// How a `gh` that has never heard of [`ALLOW_ESCAPES`] says so.
fn unknown_flag(answer: &Answer) -> bool {
    answer
        .stderr
        .contains(&format!("unknown flag: {ALLOW_ESCAPES}"))
}

/// Run one `gh` invocation whose standard output is *content* — a job's log —
/// rather than `gh`'s own words about something.
///
/// `gh` will not print content carrying terminal escape sequences unless it is
/// asked to, and a CI job's log almost always carries colour, so a log fetched
/// without asking comes back as that refusal instead. This asks, and hands back
/// what arrived without stripping anything: the caller stores a log as evidence,
/// and rendering one safely belongs to whatever prints it.
///
/// The flag goes on first and the plain call is the fallback because a `gh` that
/// does not know it rejects it while parsing its arguments, before asking GitHub
/// anything — so either generation of `gh` costs one request. The other order
/// would cost a `gh` that guards its output two.
pub fn invoke_content(args: &[&str]) -> Result<String> {
    let mut asked = args.to_vec();
    asked.push(ALLOW_ESCAPES);
    let answer = attempt(&asked)?;
    if answer.code == Some(0) {
        return Ok(answer.stdout);
    }
    if !unknown_flag(&answer) {
        return Err(refused(&asked, &answer));
    }
    invoke(args)
}

/// Parse one `gh --json` response.
pub fn json(raw: &str) -> Result<Value> {
    serde_json::from_str(raw.trim()).map_err(|e| {
        error::invalid(format!(
            "gh returned output that is not JSON ({e}): {raw:?}"
        ))
    })
}

/// The bound on waiting for the host's checks.
pub fn checks_timeout() -> Result<f64> {
    seconds(CHECKS_TIMEOUT_ENV, DEFAULT_CHECKS_TIMEOUT_SECONDS)
}

/// How long to wait between asking the host again.
pub fn checks_poll() -> Result<f64> {
    seconds(CHECKS_POLL_ENV, DEFAULT_CHECKS_POLL_SECONDS)
}

fn seconds(name: &str, default: f64) -> Result<f64> {
    let Some(raw) = std::env::var_os(name) else {
        return Ok(default);
    };
    let raw = raw.to_string_lossy().into_owned();
    let value: f64 = raw.trim().parse().map_err(|_| Error::Invalid {
        reason: format!("{name} must be a number of seconds, not {raw:?}"),
    })?;
    if !value.is_finite() || value <= 0.0 {
        return Err(Error::Invalid {
            reason: format!("{name} must be a finite number of seconds above zero, not {raw:?}"),
        });
    }
    Ok(value)
}

/// The host this implementation speaks for.
pub const HOST: &str = "github.com";

/// The `owner/name` slug an identity key spells, when it is a GitHub one.
///
/// The host is checked rather than assumed: a GitLab origin has the same three
/// segments, and handing one to `gh` would address a repository that is not there
/// under credentials that do not apply to it.
pub fn slug(identity: &str) -> Option<String> {
    let mut parts = identity.split('/');
    let host = parts.next()?;
    let owner = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() || host != HOST || owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}
