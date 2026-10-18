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
use crate::rules::WaitSeconds;

/// Names the program that stands in for `gh`.
pub const PROGRAM_ENV: &str = "ONEVCS_GH";
/// Selects the check-reading strategy, when an operator narrows it. The Actions
/// strategy also reads branch rules; unset, the complete rollup is tried first.
pub const CHECK_SOURCE_ENV: &str = "ONEVCS_CHECK_SOURCE";
/// How long a wait for the host's checks may last.
pub const CHECKS_TIMEOUT_ENV: &str = "ONEVCS_CHECKS_TIMEOUT_SECONDS";
/// How often the host is asked again while its checks are unsettled.
pub const CHECKS_POLL_ENV: &str = "ONEVCS_CHECKS_POLL_SECONDS";
/// How long a draft waits for any of its required checks to run before it is lifted
/// so they can — and, after such a lift, how long for them to run again.
pub const DRAFT_GRACE_ENV: &str = "ONEVCS_DRAFT_CHECKS_GRACE_SECONDS";
/// The default grace window. Two minutes is long past the moment a workflow that runs
/// on drafts has registered its first job, and short beside the hour-long bound.
pub const DEFAULT_DRAFT_GRACE_SECONDS: f64 = 120.0;
/// How long a draft's watch reuses a `head_activity` answer that named work in
/// progress before it asks the host again, at the default poll.
///
/// A workflow run between jobs, or an app that keeps its check suite open after its
/// runs finish, can show work in progress with no incomplete check for the whole
/// watch, and asking on every poll then costs two requests every thirty seconds for
/// an answer that moves when the work ends. Five minutes is ten polls. An empty
/// answer, a refusal, or an answer about another commit is never reused. The window
/// scales with `ONEVCS_CHECKS_POLL_SECONDS` (see [`head_activity_reuse`]), so a
/// journey that turns the poll down keeps the real ratio of polls to reuse.
pub const DEFAULT_HEAD_ACTIVITY_REUSE_SECONDS: f64 = 300.0;
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
    attempt_with_env(args, &[])
}

/// [`attempt`], with `env` set on the `gh` process — how a host query hands `gh` the
/// token its caller chose, rather than whatever `gh auth` would pick.
pub fn attempt_with_env(args: &[&str], env: &[(&str, &str)]) -> Result<Answer> {
    let output = Command::new(program())
        .args(args)
        .envs(env.iter().copied())
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

/// The process-wide bound on waiting for the host's checks, where one is set.
///
/// What overrides every identity's `checks.wait_seconds`; unset, the rules file
/// decides.
pub fn checks_timeout_override() -> Result<Option<WaitSeconds>> {
    wait(CHECKS_TIMEOUT_ENV)
}

/// How long to wait between asking the host again.
pub fn checks_poll() -> Result<f64> {
    seconds(CHECKS_POLL_ENV, DEFAULT_CHECKS_POLL_SECONDS)
}

/// How long a draft waits for its required checks to start before it is lifted.
pub fn draft_grace() -> Result<f64> {
    seconds(DRAFT_GRACE_ENV, DEFAULT_DRAFT_GRACE_SECONDS)
}

/// How long a `head_activity` answer naming work in progress is reused:
/// [`DEFAULT_HEAD_ACTIVITY_REUSE_SECONDS`] at the default poll, and the same number of
/// polls at any other.
pub fn head_activity_reuse() -> Result<f64> {
    Ok(DEFAULT_HEAD_ACTIVITY_REUSE_SECONDS * checks_poll()? / DEFAULT_CHECKS_POLL_SECONDS)
}

fn seconds(name: &str, default: f64) -> Result<f64> {
    Ok(wait(name)?.map_or(default, WaitSeconds::get))
}

/// The wait `name` sets, where it sets one: parsed and validated into the same type a
/// rules file's `checks.wait_seconds` is, so neither can carry a wait no watch runs to.
fn wait(name: &str) -> Result<Option<WaitSeconds>> {
    let Some(raw) = std::env::var_os(name) else {
        return Ok(None);
    };
    let raw = raw.to_string_lossy().into_owned();
    let value: f64 = raw.trim().parse().map_err(|_| Error::Invalid {
        reason: format!("{name} must be a number of seconds, not {raw:?}"),
    })?;
    WaitSeconds::try_from(value)
        .map(Some)
        .map_err(|_| Error::Invalid {
            reason: format!("{name} must be {}, not {raw:?}", WaitSeconds::RANGE),
        })
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

/// How the reason an [`Unanswered`] request is refused with begins.
const UNANSWERED: &str = "the host did not answer: ";

/// A request the host did not answer — rate limited, failing on its own side, or never
/// reached — as opposed to one it answered with a refusal such as `404`.
///
/// The difference is what a caller may conclude. A refusal is the host's answer about
/// this credential; a request it did not answer says nothing about the repository, and
/// is retried rather than read as anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Unanswered {
    /// What did not answer and why, with the limit and its reset where the host sent
    /// them, built from the response's status and headers alone — so it names no
    /// repository and may be printed.
    pub said: String,
    /// What `gh` wrote, which may name the repository it asked about.
    pub detail: String,
}

// llmlint: ignore-block[invalid_states_unrepresentable] the classification crosses the
// `RemoteHost::visibility -> Result<Visibility>` seam, whose signature and whose `Error`
// variants docs/contract.md fixes, so a typed carrier would be a public item the contract
// does not name. The one reader is `boundary::visibility`, which asks only of a
// `visibility()` error, and the parse is strict; a reason that merely began with this
// sentence would hold the write as unavailable, which is the direction that never lets a
// write through unchecked.
impl Unanswered {
    /// The error a [`RemoteHost`](crate::host::RemoteHost) method answers with, which
    /// [`Unanswered::of`] reads back.
    pub fn into_error(self) -> Error {
        Error::Invalid {
            reason: format!(
                "{UNANSWERED}{}\n{}",
                self.said.replace('\n', " "),
                self.detail
            ),
        }
    }

    /// The request `error` reports the host did not answer, when it is one: an
    /// [`Error::Invalid`] spelled exactly as [`Unanswered::into_error`] spells one.
    pub fn of(error: &Error) -> Option<Unanswered> {
        let Error::Invalid { reason } = error else {
            return None;
        };
        let (said, detail) = reason.strip_prefix(UNANSWERED)?.split_once('\n')?;
        if said.is_empty() {
            return None;
        }
        Some(Unanswered {
            said: said.to_owned(),
            detail: detail.to_owned(),
        })
    }

    /// Whether the `gh api --include` call that wrote `answer` is one the host did
    /// not answer, and how to say so: `api` names the endpoint without the repository.
    ///
    /// Rate limited is any `429`, and a `403` the host marks as one — no requests
    /// remaining, a `Retry-After`, or its message saying so; failing is any `5xx`;
    /// never reached is a call `gh` printed no status line for. Every other status is
    /// an answer.
    pub fn read(api: &str, answer: &Answer) -> Option<Unanswered> {
        if answer.code == Some(0) {
            return None;
        }
        let response = Response::read(&answer.stdout);
        let detail = answer.detail();
        let said = match response.status {
            None => format!("{api} gave no HTTP response"),
            Some(status) if status >= 500 => {
                format!("{api} failed on the host's side: HTTP {status}")
            }
            Some(status @ (403 | 429)) => {
                let message = format!("{detail} {}", response.body).to_lowercase();
                let remaining = response.count("x-ratelimit-remaining");
                let retry = response.count("retry-after");
                if status == 403
                    && remaining != Some(0)
                    && retry.is_none()
                    && !message.contains("rate limit")
                {
                    return None;
                }
                let which = if message.contains("secondary rate limit") {
                    "a secondary rate limit"
                } else {
                    "its rate limit"
                };
                let mut said = format!("{api} is refused for {which}: HTTP {status}");
                if let Some(limit) = response.count("x-ratelimit-limit") {
                    said.push_str(&format!(", limit {limit}"));
                    if let Some(resource) = response.word("x-ratelimit-resource") {
                        said.push_str(&format!(" ({resource})"));
                    }
                }
                if let Some(remaining) = remaining {
                    said.push_str(&format!(", {remaining} remaining"));
                }
                if let Some(reset) = response.moment("x-ratelimit-reset") {
                    said.push_str(&format!(", resets at {}", crate::ids::stamp(reset)));
                }
                if let Some(retry) = retry {
                    said.push_str(&format!(", retry after {retry}s"));
                }
                said
            }
            Some(_) => return None,
        };
        Some(Unanswered { said, detail })
    }
}
// llmlint: ignore-end[invalid_states_unrepresentable]

/// What one `gh api --include` wrote to standard output: the response's status line
/// and headers, then its body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Response {
    /// The status, where a status line came first.
    pub status: Option<u16>,
    /// Every header, as sent.
    headers: Vec<(String, String)>,
    /// The body: everything after the headers, or the whole output with no status line.
    pub body: String,
}

impl Response {
    /// Read `stdout`, which carries no status line or headers where `gh` sent no
    /// request — and is then all body.
    pub fn read(stdout: &str) -> Response {
        let Some(rest) = stdout.strip_prefix("HTTP/") else {
            return Response {
                body: stdout.to_owned(),
                ..Response::default()
            };
        };
        let mut lines = rest.split_inclusive('\n');
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok());
        let mut headers = Vec::new();
        let mut consumed = 0;
        for line in lines.by_ref() {
            consumed += line.len();
            let line = line.trim_end_matches(['\r', '\n']);
            if line.is_empty() {
                break;
            }
            if let Some((name, value)) = line.split_once(':') {
                headers.push((name.trim().to_owned(), value.trim().to_owned()));
            }
        }
        let start = rest.find('\n').map_or(rest.len(), |end| end + 1) + consumed;
        Response {
            status,
            headers,
            body: rest.get(start..).unwrap_or_default().to_owned(),
        }
    }

    /// The value of header `name`, compared without case.
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(header, _)| header.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }

    /// Header `name` as a count — a limit, a remainder, seconds, or an epoch — or
    /// nothing where it is absent or not one.
    fn count(&self, name: &str) -> Option<u64> {
        self.header(name)?.parse().ok()
    }

    /// Header `name` as a moment, sent as seconds since the Unix epoch, or nothing
    /// where it is absent or no moment this build can name.
    fn moment(&self, name: &str) -> Option<time::OffsetDateTime> {
        let epoch: i64 = self.header(name)?.parse().ok()?;
        time::OffsetDateTime::from_unix_timestamp(epoch).ok()
    }

    /// Header `name` as one short word of lower-case letters, digits and `_` — the
    /// shape GitHub names a rate limit's resource in — or nothing where it is not one.
    fn word(&self, name: &str) -> Option<&str> {
        self.header(name).filter(|value| {
            (1..=40).contains(&value.len())
                && value
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        })
    }
}
