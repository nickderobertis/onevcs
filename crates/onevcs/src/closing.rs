//! The issue-closing references a squash has to carry forward.
//!
//! A `local-direct` publication lands the branch as one commit whose message this
//! crate composes — the subject and the provenance trailers — so everything else the
//! branch's commits said is dropped, deliberately: ordinary prose is the branch's
//! history and not the base's. One kind of line is not prose. GitHub closes an issue
//! from `Closes #12`, `Fixes owner/name#12` or `Resolves <issue URL>` only in the
//! commit that reaches the default branch, so a squash that drops those lines leaves
//! every issue its work delivered open until somebody closes it by hand.
//!
//! What is collected is every keyword-and-reference pair GitHub recognises, wherever
//! in a message it sits: the keywords `close`, `fix` and `resolve` in any of their
//! inflections and any case, an optional colon, and a bare `#N`, an `owner/name#N`,
//! or an issue URL. Each is written back as one line, `<Keyword> <reference>`, so a
//! sentence that happened to close an issue survives as the reference and not as the
//! sentence.
//!
//! The same references are what a landing *reports* closing: every landing event carries
//! `closes`, the GitHub issues it closes as `owner/name#N`, and [`report`] is the one
//! place that field is written. A squash this crate composes reports the references it
//! carried, resolved to issues ([`Closing::issue`]); a merge GitHub performed reports what
//! GitHub said that change request closes, which the read that found the merge
//! [`heard`] and the record of the landing asks for ([`heard_of`]).

use std::collections::{BTreeMap, HashSet};
use std::num::NonZeroU64;
use std::sync::Mutex;

use serde_json::{Map, Value};

/// Every keyword GitHub reads as closing an issue, lowercased.
const KEYWORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// One closing reference, as it is written back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Closing {
    /// The keyword, capitalised: `Closes`, `Fixed`.
    keyword: String,
    /// The issue, as `#N`, `owner/name#N`, or the URL of an issue on a host other
    /// than `github.com`.
    reference: String,
}

impl Closing {
    /// The line a squash carries it forward as: `<Keyword> <reference>`.
    pub(crate) fn line(&self) -> String {
        format!("{} {}", self.keyword, self.reference)
    }

    /// The GitHub issue it closes, with a bare `#N` taken to be an issue of `own` —
    /// the `owner/name` of the repository the commit lands in. `None` for an issue on
    /// another host, and for a bare `#N` where the repository is not a GitHub one: the
    /// line still closes whatever its host reads it as, and is still carried, but it
    /// names no GitHub issue this crate can report.
    pub(crate) fn issue(&self, own: Option<&str>) -> Option<Issue> {
        // A URL is kept as written only where it names an issue on another host:
        // a github.com one was read as `owner/name#N`.
        if self.reference.contains("://") {
            return None;
        }
        let (repository, number) = match self.reference.strip_prefix('#') {
            Some(number) => (own?, number),
            None => self.reference.split_once('#')?,
        };
        let (owner, name) = repository.split_once('/')?;
        Issue::new(owner, name, number.parse().ok()?)
    }
}

/// One GitHub issue a landing closes, spelled `owner/name#N` as GitHub spells the
/// repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Issue {
    owner: String,
    name: String,
    /// GitHub numbers issues from 1.
    number: NonZeroU64,
}

impl Issue {
    /// An issue, where `owner` and `name` can be a GitHub repository's and `number` an
    /// issue's.
    fn new(owner: &str, name: &str, number: u64) -> Option<Self> {
        let number = NonZeroU64::new(number)?;
        (is_owner(owner) && is_name(name)).then(|| Issue {
            owner: owner.to_owned(),
            name: name.to_owned(),
            number,
        })
    }
}

impl std::fmt::Display for Issue {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}/{}#{}", self.owner, self.name, self.number)
    }
}

/// The distinct closing references `messages` carry, in the order they first appear.
///
/// Two lines closing the same issue are one: which keyword closed it does not change
/// what the host does, and a squash naming an issue twice reads as two deliveries.
pub(crate) fn references<'a>(messages: impl IntoIterator<Item = &'a str>) -> Vec<Closing> {
    let mut seen = HashSet::new();
    let mut answer = Vec::new();
    for message in messages {
        for line in message.lines() {
            for closing in in_line(line) {
                if seen.insert(closing.reference.to_lowercase()) {
                    answer.push(closing);
                }
            }
        }
    }
    answer
}

/// The GitHub issues `closings` close, a bare `#N` taken as an issue of `own`.
pub(crate) fn issues(closings: &[Closing], own: Option<&str>) -> Vec<Issue> {
    closings
        .iter()
        .filter_map(|closing| closing.issue(own))
        .collect()
}

/// Write `closes` onto a landing event's payload: `issues` as `owner/name#N`, each
/// once ignoring case, in the order each first appears, and `[]` for none.
///
/// The one place the field is spelled, so every landing a consumer relays says it
/// the same way — the shape `docs/contract.md`'s amendment fixes.
pub(crate) fn report(payload: &mut Map<String, Value>, issues: impl IntoIterator<Item = Issue>) {
    let mut seen = HashSet::new();
    let closes: Vec<Value> = issues
        .into_iter()
        .map(|issue| issue.to_string())
        .filter(|spelled| seen.insert(spelled.to_lowercase()))
        .map(Value::String)
        .collect();
    payload.insert("closes".to_owned(), Value::Array(closes));
}

/// The issues GitHub says a change request closes, out of its `closingIssuesReferences`
/// as `gh pr view --json` answers it (a list) or a GraphQL connection does
/// (`{nodes: [...]}`), or what about the answer is not that.
///
/// Refused whole rather than read in part: a list with an unreadable node in it is not
/// the list of what the change closes, and a landing reporting the rest would present
/// it as complete.
fn from_host(answered: Option<&Value>) -> std::result::Result<Vec<Issue>, String> {
    let nodes = match answered {
        Some(Value::Array(nodes)) => nodes,
        Some(connection) => connection
            .get("nodes")
            .and_then(Value::as_array)
            .ok_or_else(|| format!("{connection} is neither a list nor a connection of nodes"))?,
        None => return Err("the answer carries no closingIssuesReferences".to_owned()),
    };
    nodes
        .iter()
        .map(|node| {
            let owner = node
                .pointer("/repository/owner/login")
                .and_then(Value::as_str);
            let name = node.pointer("/repository/name").and_then(Value::as_str);
            let number = node.get("number").and_then(Value::as_u64);
            owner
                .zip(name)
                .zip(number)
                .and_then(|((owner, name), number)| Issue::new(owner, name, number))
                .ok_or_else(|| format!("{node} names no repository and issue number"))
        })
        .collect()
}

/// What a read of a merged change request heard about the issues it closes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Heard {
    /// The host's answer: these, and no others.
    Issues(Vec<Issue>),
    /// The host was asked and gave no answer this crate could read — refused the field,
    /// or answered it in a shape no issue can be read out of. The landing reports none,
    /// and nothing else stands in for the host's answer.
    Unanswered,
}

impl Heard {
    /// The issues a landing reports for what was heard: none where nothing was.
    pub(crate) fn issues(self) -> Vec<Issue> {
        match self {
            Heard::Issues(issues) => issues,
            Heard::Unanswered => Vec::new(),
        }
    }
}

/// What the last read of each merged change request heard about what it closes, by its
/// URL — the request that records a merge is the one that reads its closures, so the
/// record of the landing asks for nothing again.
static HEARD: Mutex<BTreeMap<String, Heard>> = Mutex::new(BTreeMap::new());

fn record(url: &str, heard: Heard) {
    HEARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .insert(url.to_owned(), heard);
}

/// Keep what the host answered change request `url` closes, replacing anything older —
/// or, where the answer is not one, keep that it was not and say so.
pub(crate) fn hear(url: &str, answered: Option<&Value>) {
    match from_host(answered) {
        Ok(issues) => record(url, Heard::Issues(issues)),
        Err(problem) => {
            eprintln!(
                "onevcs: warning: the host's answer about which issues {url} closes could \
                 not be read ({problem}), so its landing reports none"
            );
            record(url, Heard::Unanswered);
        }
    }
}

/// Keep that a read of change request `url` was answered without what it closes.
pub(crate) fn unanswered(url: &str) {
    record(url, Heard::Unanswered);
}

/// What the last read this process made of change request `url`'s merge heard about
/// what it closes, where one did.
pub(crate) fn heard_of(url: &str) -> Option<Heard> {
    HEARD
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(url)
        .cloned()
}

/// Every closing reference in one line.
fn in_line(line: &str) -> Vec<Closing> {
    let mut found = Vec::new();
    let mut at = 0;
    while at < line.len() {
        let rest = &line[at..];
        let word_len = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        let starts_a_word = line[..at]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if word_len > 0 && starts_a_word {
            let word = &rest[..word_len];
            if KEYWORDS.contains(&word.to_ascii_lowercase().as_str()) {
                if let Some((reference, used)) = reference_after(&rest[word_len..]) {
                    found.push(Closing {
                        keyword: capitalised(word),
                        reference,
                    });
                    at += word_len + used;
                    continue;
                }
            }
            at += word_len;
            continue;
        }
        at += rest.chars().next().map_or(1, char::len_utf8);
    }
    found
}

/// The reference a keyword is followed by, and how many bytes of `after` it took.
///
/// The keyword has to be followed by a colon, whitespace, or both — `Closes#12` is not
/// a keyword GitHub reads — and the reference has to end where the word does, so
/// `#12abc` names nothing.
fn reference_after(after: &str) -> Option<(String, usize)> {
    let colon = usize::from(after.starts_with(':'));
    let spaced = after[colon..].len() - after[colon..].trim_start().len();
    if colon + spaced == 0 {
        return None;
    }
    let start = colon + spaced;
    let candidate = &after[start..];
    let end = candidate
        .find(|c: char| c.is_whitespace() || matches!(c, ',' | ';' | ')' | ']' | '>' | '"'))
        .unwrap_or(candidate.len());
    let token = candidate[..end].trim_end_matches(['.', ':', '!', '?']);
    let reference = issue(token)?;
    Some((reference, start + token.len()))
}

/// The issue one token names, normalised, or `None` when it names none.
fn issue(token: &str) -> Option<String> {
    if let Some(number) = token.strip_prefix('#') {
        return is_number(number).then(|| format!("#{number}"));
    }
    if let Some(url) = token
        .strip_prefix("https://")
        .or_else(|| token.strip_prefix("http://"))
    {
        let mut parts = url.split('/');
        let (host, owner, name, kind, number) = (
            parts.next()?,
            parts.next()?,
            parts.next()?,
            parts.next()?,
            parts.next()?,
        );
        if parts.next().is_some()
            || kind != "issues"
            || !is_owner(owner)
            || !is_name(name)
            || !is_number(number)
        {
            return None;
        }
        // One spelling per issue, so a URL and the short form of the same issue are
        // recognised as one. A host other than github.com has no short form here.
        return Some(if host.eq_ignore_ascii_case("github.com") {
            format!("{owner}/{name}#{number}")
        } else {
            token.to_owned()
        });
    }
    let (repository, number) = token.split_once('#')?;
    let (owner, name) = repository.split_once('/')?;
    (is_owner(owner) && is_name(name) && is_number(number))
        .then(|| format!("{owner}/{name}#{number}"))
}

fn is_number(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_digit())
}

fn is_owner(text: &str) -> bool {
    !text.is_empty() && text.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-')
}

fn is_name(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}

fn capitalised(word: &str) -> String {
    let lower = word.to_ascii_lowercase();
    let mut chars = lower.chars();
    chars.next().map_or_else(String::new, |first| {
        first.to_ascii_uppercase().to_string() + chars.as_str()
    })
}

#[cfg(test)]
mod tests {
    fn lines<'a>(messages: impl IntoIterator<Item = &'a str>) -> Vec<String> {
        super::references(messages)
            .iter()
            .map(super::Closing::line)
            .collect()
    }

    #[test]
    fn every_form_github_reads_is_collected_once_in_order() {
        let messages = [
            "feat: add it\n\nSome prose that fixes nothing.\n\nCloses owner/name#1071\nfixes: #7",
            "fix: more\n\nThis resolves https://github.com/owner/name/issues/1071 and CLOSED #8.",
            "chore: tidy\n\nResolves #7, resolves other/repo#3",
        ];
        assert_eq!(
            lines(messages),
            vec![
                "Closes owner/name#1071",
                "Fixes #7",
                "Closed #8",
                "Resolves other/repo#3",
            ]
        );
    }

    #[test]
    fn nothing_that_only_resembles_a_reference_is_collected() {
        let messages = [
            "fix: the #12 thing\n\nprefixes #3\nCloses#4\ncloses #5abc\nfixes owner/name#\n\
             closes https://github.com/owner/name/pull/9\nclose issue #6",
        ];
        assert!(lines(messages).is_empty(), "{:?}", lines(messages));
    }
}
