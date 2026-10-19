//! What a journey asks the substituted `gh`, held to what the same journey asked before
//! a change that must add no request.
//!
//! Reporting the issues a landing closes reads GitHub's `closingIssuesReferences` in the
//! request that already records the merge, and nowhere else. No answer can show that —
//! a host asked twice answers the same as one asked once — so what is held is the call
//! log the fake `gh` writes (`gh-calls.log`): every distinct invocation a journey makes
//! occurs no more times than it did in the same journey's log recorded on the code
//! before the change, under `tests/fixtures/gh-call-baselines/`. A read that only gains
//! the `closingIssuesReferences` field, or its selection set in a GraphQL query, is the
//! same invocation; nothing else is normalised.
//!
//! A baseline is re-recorded only on purpose, on behaviour that has not moved — never to
//! absorb a new call. `tests/fixtures/gh-call-baselines/README.md` says how each was made.

use std::collections::BTreeMap;
use std::path::PathBuf;

use crate::world::World;

/// The field, and the GraphQL connection, that reading a merge's closed issues adds.
const CLOSES_FIELD: &str = "closingIssuesReferences";

/// One call as an invocation: the call with the closed-issues field taken out of its
/// `--json` field list and its selection set taken out of a GraphQL query.
fn invocation(call: &str) -> String {
    let mut text = call.trim().to_owned();
    // A GraphQL selection: the field, its arguments, and its whole selection set.
    while let Some(start) = text.find(&format!("{CLOSES_FIELD}(")) {
        let Some(open) = text[start..].find('{').map(|at| start + at) else {
            break;
        };
        let mut depth = 0;
        let mut end = None;
        for (at, character) in text[open..].char_indices() {
            match character {
                '{' => depth += 1,
                '}' => {
                    depth -= 1;
                    if depth == 0 {
                        end = Some(open + at + 1);
                        break;
                    }
                }
                _ => {}
            }
        }
        let Some(end) = end else { break };
        let end = if text[end..].starts_with(' ') {
            end + 1
        } else {
            end
        };
        text.replace_range(start..end, "");
    }
    // A `gh … --json` field list.
    text.split(' ')
        .map(|word| {
            if word.split(',').any(|field| field == CLOSES_FIELD) {
                word.split(',')
                    .filter(|field| *field != CLOSES_FIELD)
                    .collect::<Vec<_>>()
                    .join(",")
            } else {
                word.to_owned()
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// How many times each invocation occurs in `calls`.
fn tally<'a>(calls: impl IntoIterator<Item = &'a str>) -> BTreeMap<String, usize> {
    let mut counted = BTreeMap::new();
    for call in calls {
        let call = call.trim();
        if call.is_empty() {
            continue;
        }
        *counted.entry(invocation(call)).or_insert(0) += 1;
    }
    counted
}

fn baseline_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/gh-call-baselines")
        .join(format!("{name}.log"))
}

/// Hold `world`'s call log to the baseline named `name`: no invocation occurs more
/// times than it does there, so a new invocation, or one more of an old one, fails.
pub fn assert_no_call_beyond(world: &World, name: &str) {
    let path = baseline_path(name);
    let recorded = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the baseline {} is committed: {e}", path.display()));
    let allowed = tally(recorded.lines());
    let calls = world.host_calls();
    let made = tally(calls.iter().map(String::as_str));
    let beyond: Vec<String> = made
        .iter()
        .filter(|(call, count)| **count > allowed.get(*call).copied().unwrap_or(0))
        .map(|(call, count)| {
            format!(
                "{count}× (baseline {}×) {call}",
                allowed.get(call).copied().unwrap_or(0)
            )
        })
        .collect();
    assert!(
        beyond.is_empty(),
        "the journey asked the host for more than {} records it asking:\n{}\n\nits whole log:\n{}",
        path.display(),
        beyond.join("\n"),
        calls.join("\n"),
    );
}

/// Hold `world`'s call log to the baseline named `name` exactly: the same invocations,
/// each as many times — for a path that must ask the host precisely what it asked.
pub fn assert_calls_unchanged(world: &World, name: &str) {
    let path = baseline_path(name);
    let recorded = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the baseline {} is committed: {e}", path.display()));
    let calls = world.host_calls();
    assert_eq!(
        tally(calls.iter().map(String::as_str)),
        tally(recorded.lines()),
        "the journey's host calls differ from {}; its whole log:\n{}",
        path.display(),
        calls.join("\n"),
    );
}

/// Hold `world`'s call log to the baseline named `name` plus exactly one more `call`:
/// every invocation as many times as there, except that one, once more — for the one
/// path allowed a request the baseline does not make.
pub fn assert_one_call_beyond(world: &World, name: &str, call: &str) {
    let path = baseline_path(name);
    let recorded = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("the baseline {} is committed: {e}", path.display()));
    let mut allowed = tally(recorded.lines());
    *allowed.entry(invocation(call)).or_insert(0) += 1;
    let calls = world.host_calls();
    assert_eq!(
        tally(calls.iter().map(String::as_str)),
        allowed,
        "the journey's host calls are not {} and one more {call:?}; its whole log:\n{}",
        path.display(),
        calls.join("\n"),
    );
}
