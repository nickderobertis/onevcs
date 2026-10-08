//! A read-only GitHub client that answers in coverage statuses.
//!
//! Every response is reduced to a value or a [`Status`]; the body of a refusal is
//! never kept, because it can quote the repository it refused. Only `GET` and
//! GraphQL queries are issued — a document that is not a query is refused before it
//! is sent — and once the host has refused for quota, every later call answers
//! `rate-limited` without asking again, since each further attempt extends a
//! secondary limit nothing reports.

use std::cell::{Cell, RefCell};
use std::time::{Duration, Instant};

use serde::Serialize;
use serde_json::{json, Value};

use crate::status::Status;

/// What the run spent against the host.
#[derive(Clone, Debug, Default, Serialize)]
pub struct ApiStats {
    /// REST requests issued, `/rate_limit` excluded (it costs no quota).
    pub rest_requests: u64,
    /// GraphQL requests issued.
    pub graphql_requests: u64,
    /// The sum of the `rateLimit.cost` each GraphQL response reported.
    pub graphql_cost: u64,
    /// `/rate_limit` reads, which GitHub does not count against a limit.
    pub rate_limit_reads: u64,
    /// Requests answered with a quota refusal.
    pub rate_limited_responses: u64,
    /// Calls answered `rate-limited` without a request, after a quota refusal.
    pub short_circuited: u64,
    /// Wall time spent waiting on the host, in milliseconds.
    pub wait_ms: u64,
}

/// The two quotas a run spends.
#[derive(Clone, Copy, Debug, Default, Serialize)]
pub struct Quota {
    pub core_limit: u64,
    pub core_remaining: u64,
    pub graphql_limit: u64,
    pub graphql_remaining: u64,
}

/// One REST response's JSON.
pub struct Page {
    pub body: Value,
}

pub struct Api {
    base: String,
    token: String,
    agent: ureq::Agent,
    stats: RefCell<ApiStats>,
    tripped: Cell<bool>,
}

impl Api {
    pub fn new(base: &str, token: String) -> Api {
        let mut tls = ureq::tls::TlsConfig::builder()
            .provider(ureq::tls::TlsProvider::Rustls)
            .unversioned_rustls_crypto_provider(std::sync::Arc::new(
                rustls::crypto::ring::default_provider(),
            ));
        let roots = rustls_native_certs::load_native_certs().certs;
        if !roots.is_empty() {
            let roots: Vec<_> = roots
                .iter()
                .map(|cert| ureq::tls::Certificate::from_der(cert.as_ref()).to_owned())
                .collect();
            tls = tls.root_certs(ureq::tls::RootCerts::new_with_certs(&roots));
        }
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .tls_config(tls.build())
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_secs(120)))
            .user_agent("onevcs-exposure-audit")
            .build()
            .into();
        Api {
            base: base.trim_end_matches('/').to_owned(),
            token,
            agent,
            stats: RefCell::new(ApiStats::default()),
            tripped: Cell::new(false),
        }
    }

    pub fn stats(&self) -> ApiStats {
        self.stats.borrow().clone()
    }

    fn url(&self, path_or_url: &str) -> String {
        if path_or_url.starts_with("http://") || path_or_url.starts_with("https://") {
            path_or_url.to_owned()
        } else {
            format!("{}{}", self.base, path_or_url)
        }
    }

    /// The quota left, read from the endpoint that costs none of it.
    pub fn quota(&self) -> Option<Quota> {
        self.stats.borrow_mut().rate_limit_reads += 1;
        let mut response = self
            .agent
            .get(&self.url("/rate_limit"))
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .call()
            .ok()?;
        if response.status().as_u16() != 200 {
            return None;
        }
        let body: Value = serde_json::from_str(&response.body_mut().read_to_string().ok()?).ok()?;
        let resources = body.get("resources")?;
        let read = |name: &str, field: &str| resources.get(name)?.get(field)?.as_u64();
        Some(Quota {
            core_limit: read("core", "limit")?,
            core_remaining: read("core", "remaining")?,
            graphql_limit: read("graphql", "limit").unwrap_or(0),
            graphql_remaining: read("graphql", "remaining").unwrap_or(0),
        })
    }

    /// One REST `GET`.
    pub fn get(&self, path_or_url: &str) -> Result<Page, Status> {
        if self.tripped.get() {
            self.stats.borrow_mut().short_circuited += 1;
            return Err(Status::RateLimited);
        }
        self.stats.borrow_mut().rest_requests += 1;
        let started = Instant::now();
        let result = self
            .agent
            .get(&self.url(path_or_url))
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .call();
        self.stats.borrow_mut().wait_ms += elapsed_ms(started);
        let mut response = result.map_err(|_| Status::OtherError)?;
        let status = response.status().as_u16();
        let header = |name: &str| {
            response
                .headers()
                .get(name)
                .and_then(|v| v.to_str().ok())
                .map(str::to_owned)
        };
        let remaining_zero = header("x-ratelimit-remaining").as_deref() == Some("0");
        let retry_after = header("retry-after").is_some();
        if status != 200 {
            let refused = classify_http(status, remaining_zero || retry_after);
            if refused == Status::RateLimited {
                self.trip();
            }
            return Err(refused);
        }
        let text = response
            .body_mut()
            .with_config()
            .limit(256 * 1024 * 1024)
            .read_to_string()
            .map_err(|_| Status::OtherError)?;
        let body = serde_json::from_str(&text).map_err(|_| Status::OtherError)?;
        Ok(Page { body })
    }

    /// One GraphQL query. The document must be a query: this client never mutates.
    pub fn graphql(&self, query: &str, variables: Value) -> Result<Value, Status> {
        self.graphql_with(&self.token, query, variables)
    }

    /// One GraphQL query under another credential (a board's).
    pub fn graphql_with(
        &self,
        token: &str,
        query: &str,
        variables: Value,
    ) -> Result<Value, Status> {
        assert!(
            query.trim_start().starts_with("query"),
            "the audit issues GraphQL queries only"
        );
        if self.tripped.get() {
            self.stats.borrow_mut().short_circuited += 1;
            return Err(Status::RateLimited);
        }
        if token.is_empty() {
            return Err(Status::PermissionDenied);
        }
        self.stats.borrow_mut().graphql_requests += 1;
        let started = Instant::now();
        let payload = json!({ "query": query, "variables": variables }).to_string();
        let result = self
            .agent
            .post(&self.url("/graphql"))
            .header("Authorization", &format!("Bearer {token}"))
            .header("Content-Type", "application/json")
            .send(payload.as_str());
        self.stats.borrow_mut().wait_ms += elapsed_ms(started);
        let mut response = result.map_err(|_| Status::OtherError)?;
        let status = response.status().as_u16();
        let quota_header = response
            .headers()
            .get("x-ratelimit-remaining")
            .and_then(|v| v.to_str().ok())
            == Some("0")
            || response.headers().get("retry-after").is_some();
        if status != 200 {
            let refused = classify_http(status, quota_header);
            if refused == Status::RateLimited {
                self.trip();
            }
            return Err(refused);
        }
        let text = response
            .body_mut()
            .with_config()
            .limit(256 * 1024 * 1024)
            .read_to_string()
            .map_err(|_| Status::OtherError)?;
        let body: Value = serde_json::from_str(&text).map_err(|_| Status::OtherError)?;
        if let Some(errors) = body.get("errors").and_then(Value::as_array) {
            if !errors.is_empty() {
                let refused = classify_graphql(errors);
                if refused == Status::RateLimited {
                    self.trip();
                }
                return Err(refused);
            }
        }
        let data = body.get("data").cloned().unwrap_or(Value::Null);
        if let Some(cost) = data.pointer("/rateLimit/cost").and_then(Value::as_u64) {
            self.stats.borrow_mut().graphql_cost += cost;
        }
        Ok(data)
    }

    fn trip(&self) {
        self.tripped.set(true);
        self.stats.borrow_mut().rate_limited_responses += 1;
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// A refused HTTP status, as a coverage word.
fn classify_http(status: u16, quota_signal: bool) -> Status {
    match status {
        429 => Status::RateLimited,
        403 if quota_signal => Status::RateLimited,
        401 | 403 => Status::PermissionDenied,
        404 | 410 | 451 => Status::NotFound,
        _ => Status::OtherError,
    }
}

/// A GraphQL `errors` array, as a coverage word: the worst type named in it.
fn classify_graphql(errors: &[Value]) -> Status {
    errors
        .iter()
        .map(
            |error| match error.get("type").and_then(Value::as_str).unwrap_or("") {
                "RATE_LIMITED" | "RATE_LIMIT" => Status::RateLimited,
                "FORBIDDEN" | "INSUFFICIENT_SCOPES" | "UNAUTHORIZED" => Status::PermissionDenied,
                "NOT_FOUND" => Status::NotFound,
                _ => Status::OtherError,
            },
        )
        .fold(Status::Scanned, Status::combine)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refusals_map_onto_the_vocabulary() {
        assert_eq!(classify_http(403, true), Status::RateLimited);
        assert_eq!(classify_http(403, false), Status::PermissionDenied);
        assert_eq!(classify_http(404, false), Status::NotFound);
        assert_eq!(classify_http(502, false), Status::OtherError);
        let errors = [json!({"type": "INSUFFICIENT_SCOPES"}), json!({"type": "X"})];
        assert_eq!(classify_graphql(&errors), Status::PermissionDenied);
    }
}
