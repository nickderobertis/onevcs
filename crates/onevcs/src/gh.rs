//! Driving GitHub through the `gh` CLI.
//!
//! This is the one boundary an offline gate cannot exercise for free, so it is kept
//! narrow and behind one seam: every call goes through [`invoke`], and the program
//! it runs is `ONEVCS_GH` when that names one. A journey therefore fakes GitHub's
//! *decisioning* — which change requests exist, what its checks say, whether a
//! merge is allowed — while the merge itself is performed with real git against a
//! real origin. Nothing about the repository side is simulated.

use std::path::PathBuf;
use std::process::{Command, Stdio};

use serde_json::Value;

use crate::error::{self, Error, Result};

/// Names the program that stands in for `gh`.
pub const PROGRAM_ENV: &str = "ONEVCS_GH";
/// How long a wait for the host's checks may last.
pub const CHECKS_TIMEOUT_ENV: &str = "ONEVCS_CHECKS_TIMEOUT_SECONDS";
/// How often the host is asked again while its checks are unsettled.
pub const CHECKS_POLL_ENV: &str = "ONEVCS_CHECKS_POLL_SECONDS";
/// The default bound on waiting for required checks. Long, because a repository's
/// CI is doing the work and abandoning it mid-flight leaves state nobody recorded.
pub const DEFAULT_CHECKS_TIMEOUT_SECONDS: f64 = 3600.0;
/// The default interval between asking the host again.
pub const DEFAULT_CHECKS_POLL_SECONDS: f64 = 5.0;

/// The program that answers as `gh`.
pub fn program() -> PathBuf {
    std::env::var_os(PROGRAM_ENV)
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("gh"))
}

/// Run one `gh` invocation and return its standard output.
pub fn invoke(args: &[&str]) -> Result<String> {
    let output = Command::new(program())
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(error::at("run", &program()))?;
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    if output.status.success() {
        return Ok(stdout);
    }
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let detail = if stderr.trim().is_empty() {
        stdout.trim().to_owned()
    } else {
        stderr.trim().to_owned()
    };
    Err(Error::Invalid {
        reason: format!("gh {} failed: {detail}", args.join(" ")),
    })
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

/// The GitHub `owner/name` slug an identity key spells.
pub fn slug(identity: &str) -> Option<String> {
    let mut parts = identity.split('/');
    let host = parts.next()?;
    let owner = parts.next()?;
    let name = parts.next()?;
    if parts.next().is_some() || host.is_empty() || owner.is_empty() || name.is_empty() {
        return None;
    }
    Some(format!("{owner}/{name}"))
}
