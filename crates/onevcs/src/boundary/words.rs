//! The generic-word list a bare or package term is narrowed by.
//!
//! A repository called `docs`, or a package called `server`, would refuse every
//! public write that used the word, so a name that is an ordinary English or software
//! word is matched only as the fully qualified `owner/name`. The list is data rather
//! than code — `generic-words.txt` beside this file, versioned by its own header — so
//! it can grow without a change here, and the version it is at is part of what
//! `docs/contract.md` states.

use std::collections::HashSet;
use std::sync::OnceLock;

/// The list, as shipped.
const LIST: &str = include_str!("generic-words.txt");

fn words() -> &'static HashSet<&'static str> {
    static WORDS: OnceLock<HashSet<&'static str>> = OnceLock::new();
    WORDS.get_or_init(|| {
        LIST.lines()
            .map(str::trim)
            .filter(|line| !line.is_empty() && !line.starts_with('#'))
            .collect()
    })
}

/// Whether `word`, already in lower case, is on the list.
pub fn is_generic(word: &str) -> bool {
    words().contains(word)
}
