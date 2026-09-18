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

use std::collections::HashSet;

/// Every keyword GitHub reads as closing an issue, lowercased.
const KEYWORDS: [&str; 9] = [
    "close", "closes", "closed", "fix", "fixes", "fixed", "resolve", "resolves", "resolved",
];

/// One closing reference, as it is written back.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Closing {
    /// The keyword, capitalised: `Closes`, `Fixed`.
    keyword: String,
    /// The issue, as `#N`, `owner/name#N`, or the URL of an issue on a host other
    /// than `github.com`.
    reference: String,
}

/// The distinct closing lines `messages` carry, in the order they first appear.
///
/// Two lines closing the same issue are one: which keyword closed it does not change
/// what the host does, and a squash naming an issue twice reads as two deliveries.
pub(crate) fn lines<'a>(messages: impl IntoIterator<Item = &'a str>) -> Vec<String> {
    let mut seen = HashSet::new();
    let mut answer = Vec::new();
    for message in messages {
        for line in message.lines() {
            for closing in in_line(line) {
                if seen.insert(closing.reference.to_lowercase()) {
                    answer.push(format!("{} {}", closing.keyword, closing.reference));
                }
            }
        }
    }
    answer
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
    use super::lines;

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
