//! How a refusal names the command that resolves it, and carries text a program
//! wrote.
//!
//! Every refusal on the publication path ends in an invocation an operator or an
//! agent is meant to run, so the invocation has to survive being run: a checkout
//! under a path with a space in it, or a branch name git accepted that a shell
//! would split, must come back out of the message as the one argument it went in
//! as. A command that has to be repaired before it works is a refusal that names
//! no command, which is the thing this whole surface exists to stop.

use icu_properties::props::GeneralCategory;
use icu_properties::CodePointMapData;

/// One runnable invocation, from the arguments it is made of.
pub fn command<'a>(argv: impl IntoIterator<Item = &'a str>) -> String {
    argv.into_iter().map(word).collect::<Vec<_>>().join(" ")
}

/// One argument, quoted where a shell would otherwise split or expand it.
///
/// Single quotes, because inside them a POSIX shell expands nothing at all — the
/// only character that needs handling is the quote itself, and it is closed,
/// escaped, and reopened the way `printf %q` does it.
fn word(value: &str) -> String {
    if !value.is_empty() && value.chars().all(bare) {
        return value.to_owned();
    }
    format!("'{}'", value.replace('\'', r"'\''"))
}

/// Whether a character carries no meaning to a shell wherever it appears.
///
/// Deliberately a small allow-list rather than a list of what to escape: a
/// character nobody thought about is quoted, which is safe, instead of being
/// passed through, which is not. `~` is not on it — it is only special leading, and
/// a path that starts with one is exactly the case that would break.
fn bare(c: char) -> bool {
    c.is_ascii_alphanumeric() || "-_./=:+,@".contains(c)
}

/// How many names a refusal spells out before it says how many more there are.
///
/// Ten, because the list is read to decide what to do next: a conflict across a
/// dozen files and a conflict across two hundred call for the same first move, and
/// a refusal nobody reads to the end names nothing at all.
pub const LISTED_LIMIT: usize = 10;

/// A list of names as a refusal spells it — bounded, and counting what it dropped.
///
/// The values arrive from outside (git's own answer about what it left unmerged),
/// so the length is not this crate's to assume. What is left out is *counted*
/// rather than quietly cut: a truncated list read as the whole one is a report of a
/// smaller problem than the one that happened.
pub fn listed<S: AsRef<str>>(values: &[S]) -> String {
    let mut spelled: Vec<String> = values
        .iter()
        .take(LISTED_LIMIT)
        .map(|value| format!("{:?}", value.as_ref()))
        .collect();
    if spelled.is_empty() {
        return "no path git named".to_owned();
    }
    let dropped = values.len().saturating_sub(LISTED_LIMIT);
    if dropped > 0 {
        spelled.push(format!("and {dropped} more"));
    }
    spelled.join(", ")
}

/// Text some other program wrote, rendered for a message a terminal prints.
///
/// A repository's own hook decides what it says, and a refusal hands that back
/// whole so an operator reads the policy rather than a paraphrase of it — which
/// makes it untrusted input on its way to a terminal. An escape sequence in it can
/// move the cursor, repaint what was already written, or hide the refusal
/// altogether, so what reaches the terminal is exactly the printable text plus the
/// two characters a program lays its message out with. CRLF is folded to LF first,
/// so a hook written on Windows reads as it was written and a lone carriage return
/// is still shown as what it is.
pub fn quoted_output(value: &str) -> String {
    value
        .replace("\r\n", "\n")
        .chars()
        .map(|c| match c {
            '\n' | '\t' => c.to_string(),
            c if c.is_control() || formatting(c) => format!("\\u{{{:04x}}}", c as u32),
            c => c.to_string(),
        })
        .collect()
}

/// Whether a character changes how the text around it renders without rendering as
/// anything itself.
///
/// The other half of the problem an escape sequence is, and the half
/// `char::is_control` does not reach: it answers for `Cc` alone, so every one of
/// these arrives as an ordinary character. U+202E reverses every word after it and
/// U+2069 closes an isolate a hook never opened, so a refusal can be composed to be
/// read as its own opposite; U+200B and the tag characters take up no width at all,
/// so one can be padded with text no terminal shows.
///
/// Unicode's own name for that population is the `Cf` general category, and this
/// asks for it rather than keeping a copy of it. A copy is what this function used
/// to be — code points typed out here, right on the day and with nothing to notice
/// the day the category moved. `icu_properties` is already in the graph behind
/// `url`'s IDNA tables, so the table it answers from costs this crate no dependency
/// it did not already have and cannot drift from Unicode's.
fn formatting(c: char) -> bool {
    CodePointMapData::<GeneralCategory>::new().get(c) == GeneralCategory::Format
}
