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

/// Whether a character renders as nothing itself while changing what renders
/// around it.
///
/// The other half of the same problem an escape sequence is, and the half
/// `char::is_control` does not reach: it answers for `Cc` alone, so U+202E arrives
/// as an ordinary character and reverses every word after it, U+2069 closes an
/// isolate a hook never opened, and U+200B and the U+E0020 tag characters take up
/// no width at all. A refusal that can be read backwards, or that ends wherever the
/// text it refused decided it does, is the concealment the escaping above is here
/// to stop, so these are shown as their code points too.
///
/// The set is Unicode's `Cf` category, whole rather than the few that are famous:
/// picking out the ones somebody thought of is how the next one gets through.
/// Compare it against the category, not against a list of attacks.
fn formatting(c: char) -> bool {
    matches!(
        c as u32,
        0x00ad
            | 0x0600..=0x0605
            | 0x061c
            | 0x06dd
            | 0x070f
            | 0x0890..=0x0891
            | 0x08e2
            | 0x180e
            | 0x200b..=0x200f
            | 0x202a..=0x202e
            | 0x2060..=0x2064
            | 0x2066..=0x206f
            | 0xfeff
            | 0xfff9..=0xfffb
            | 0x110bd
            | 0x110cd
            | 0x13430..=0x1343f
            | 0x1bca0..=0x1bca3
            | 0x1d173..=0x1d17a
            | 0xe0001
            | 0xe0020..=0xe007f
    )
}
