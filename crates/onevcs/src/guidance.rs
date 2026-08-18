//! How a refusal names the command that resolves it, and carries text a program
//! wrote.
//!
//! Every refusal on the publication path ends in an invocation an operator or an
//! agent is meant to run, so the invocation has to survive being run: a checkout
//! under a path with a space in it, or a branch name git accepted that a shell
//! would split, must come back out of the message as the one argument it went in
//! as. A command that has to be repaired before it works is a refusal that names
//! no command, which is the thing this whole surface exists to stop.

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
/// `char::is_control` does not reach — it answers for `Cc` alone, so every one of
/// these arrives as an ordinary character. Three closed sets, each named rather
/// than a slice of a category that grows:
///
/// - The **explicit bidirectional formatting characters** (UAX #9): U+202E reverses
///   every word after it and U+2069 closes an isolate a hook never opened, so a
///   refusal can be composed to be read as its own opposite. Unicode fixed this set
///   at twelve in 6.3 and has added none since.
/// - The **zero-width characters** in U+200B-U+200D, U+2060, and U+FEFF, which take
///   up no width: a refusal can be padded with text no terminal shows.
/// - The **tag characters**, the whole U+E0000 block, which is the same trick with
///   a whole alphabet behind it — and a block is closed by definition.
///
/// Deliberately not "Unicode's `Cf` category": that is a moving classification, and
/// mirroring it here by hand would be a copy of somebody else's table with nothing
/// to notice when it moved. What each set above is for is stated, so a character
/// somebody wants added arrives with the reason it belongs to one of them.
// llmlint: ignore[contracts_have_one_source_or_a_drift_gate] there is no upstream
// table for a gate to reconcile against: each of the three sets is closed, and that
// is why they are the sets. UAX #9 fixed the explicit bidirectional formatting
// characters at twelve in Unicode 6.3 and has added none in the decade since, and
// U+E0000 is a block. The version of this that did mirror a moving classification —
// `Cf` whole — is what this replaced, for exactly the reason this rule names.
fn formatting(c: char) -> bool {
    matches!(
        c as u32,
        0x061c | 0x200b..=0x200f | 0x202a..=0x202e | 0x2060 | 0x2066..=0x2069 | 0xfeff
            | 0xe0000..=0xe007f
    )
}
