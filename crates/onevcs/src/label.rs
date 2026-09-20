//! The labels a caller stamps on a session, and the one grammar they are read by.
//!
//! A label is the join between a session and whatever opened it — a run, a node, a
//! launching process — and it lives on the session record because that is the one
//! place a reader of `recoverable` can reach without joining anything else. What a
//! key *means* is the caller's to declare; what this module decides is only what a
//! key and a value may be, so that a record carries nothing a command line could
//! not spell back as `--label KEY=VALUE` and a filter over one compares strings that
//! were read the same way they were written.

use std::collections::BTreeMap;

use crate::error::{self, Result};

/// One `KEY=VALUE` as a command line spells it, checked and split.
///
/// The value is everything after the first `=`, so a value may carry one of its
/// own; the key may not, because a key is what a filter is written against.
pub(crate) fn parse(spec: &str) -> Result<(String, String)> {
    let Some((key, value)) = spec.split_once('=') else {
        return Err(error::invalid(format!(
            "{spec:?} is not a label; a label is spelled KEY=VALUE"
        )));
    };
    check(key, value)?;
    Ok((key.to_owned(), value.to_owned()))
}

/// Every `KEY=VALUE` a command line supplied, as the map a record stores.
///
/// A key given twice is refused rather than last-one-wins: a caller stamping a run
/// twice with two values has said two things, and a record that kept one of them
/// would be a record that quietly disagreed with the command that wrote it.
pub(crate) fn parse_all(specs: &[String]) -> Result<BTreeMap<String, String>> {
    let mut labels = BTreeMap::new();
    for spec in specs {
        let (key, value) = parse(spec)?;
        if labels.contains_key(&key) {
            return Err(error::invalid(format!(
                "the label key {key:?} is given twice; a session carries one value per key"
            )));
        }
        labels.insert(key, value);
    }
    Ok(labels)
}

/// Refuse a map a library caller supplied that a command line could not have
/// spelled, so the record and the row read the same whichever way they were written.
pub(crate) fn validate(labels: &BTreeMap<String, String>) -> Result<()> {
    for (key, value) in labels {
        check(key, value)?;
    }
    Ok(())
}

/// Whether the labels a row carries satisfy every pair a filter asked for.
pub(crate) fn matches(
    carried: &BTreeMap<String, String>,
    wanted: &BTreeMap<String, String>,
) -> bool {
    wanted
        .iter()
        .all(|(key, value)| carried.get(key) == Some(value))
}

/// The grammar: a key is `[A-Za-z0-9_-]+`, and a value is any string without a
/// newline — a record is one JSON document per line nowhere, but a label is printed
/// on one line of a human rendering and a newline inside one would end it early.
fn check(key: &str, value: &str) -> Result<()> {
    if key.is_empty()
        || !key
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    {
        return Err(error::invalid(format!(
            "{key:?} is not a label key; a key is one or more of A-Z, a-z, 0-9, `_` and `-`"
        )));
    }
    if value.contains('\n') {
        return Err(error::invalid(format!(
            "the value of label {key:?} holds a newline; a label's value is one line"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_value_may_hold_an_equals_sign_and_a_key_may_not() {
        assert_eq!(
            parse("run=a=b").expect("the first `=` splits"),
            ("run".to_owned(), "a=b".to_owned())
        );
        assert!(parse("no separator").is_err());
        assert!(parse("=value").is_err());
        assert!(parse("bad key=value").is_err());
        assert!(parse("run=two\nlines").is_err());
    }

    #[test]
    fn a_repeated_key_is_refused_and_an_empty_value_is_not() {
        assert!(parse_all(&["run=1".to_owned(), "run=2".to_owned()]).is_err());
        let labels = parse_all(&["run=".to_owned(), "node=x".to_owned()]).expect("two labels");
        assert_eq!(labels.get("run").map(String::as_str), Some(""));
        assert!(matches(
            &labels,
            &BTreeMap::from([("node".to_owned(), "x".to_owned())])
        ));
        assert!(!matches(
            &labels,
            &BTreeMap::from([("node".to_owned(), "y".to_owned())])
        ));
        assert!(matches(&labels, &BTreeMap::new()));
    }
}
