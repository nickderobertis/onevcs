//! How a refusal names the command that resolves it.
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
