//! The identities the audit is handed or reads back: validated once where they
//! enter, from a flag or from the host, and carried typed from there.

use std::fmt;

/// A GitHub account login: letters, digits and `-`, 1 to 39 characters.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Login(String);

impl Login {
    pub fn parse(s: &str) -> Option<Login> {
        let ok = !s.is_empty()
            && s.len() <= 39
            && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-');
        ok.then(|| Login(s.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for Login {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A repository: its owner and its name, which is letters, digits, `-`, `_` and `.`.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct RepoId {
    owner: Login,
    name: String,
}

impl RepoId {
    pub fn new(owner: &str, name: &str) -> Option<RepoId> {
        let ok = !name.is_empty()
            && name.len() <= 100
            && name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b));
        Some(RepoId {
            owner: Login::parse(owner)?,
            name: ok.then(|| name.to_owned())?,
        })
    }

    /// `OWNER/NAME`.
    pub fn parse(s: &str) -> Option<RepoId> {
        let (owner, name) = s.split_once('/')?;
        RepoId::new(owner, name)
    }

    pub fn owner(&self) -> &Login {
        &self.owner
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// The case-folded `owner/name` GitHub compares identities by.
    pub fn key(&self) -> String {
        self.to_string().to_ascii_lowercase()
    }
}

impl fmt::Display for RepoId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.name)
    }
}

/// A board: a GitHub Project, by its owner and number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BoardId {
    pub owner: Login,
    pub number: u64,
}

impl BoardId {
    /// `OWNER/NUMBER`.
    pub fn parse(s: &str) -> Option<BoardId> {
        let (owner, number) = s.split_once('/')?;
        Some(BoardId {
            owner: Login::parse(owner)?,
            number: number.parse().ok().filter(|n| *n > 0)?,
        })
    }
}

impl fmt::Display for BoardId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.owner, self.number)
    }
}

/// A credential. Its `Debug` never shows it.
#[derive(Clone, PartialEq, Eq)]
pub struct Token(String);

impl Token {
    /// A non-blank credential, trimmed.
    pub fn new(value: &str) -> Option<Token> {
        let value = value.trim();
        (!value.is_empty()).then(|| Token(value.to_owned()))
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for Token {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Token(<redacted>)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identities_are_validated_where_they_enter() {
        assert_eq!(
            RepoId::parse("sample-owner/alpha.rs")
                .map(|r| r.to_string())
                .as_deref(),
            Some("sample-owner/alpha.rs")
        );
        assert!(RepoId::parse("sample-owner").is_none());
        assert!(RepoId::parse("sample owner/alpha").is_none());
        assert!(RepoId::parse("sample-owner/al/pha").is_none());
        assert_eq!(BoardId::parse("sample-owner/2").map(|b| b.number), Some(2));
        assert!(BoardId::parse("sample-owner/zero").is_none());
        assert!(BoardId::parse("sample-owner/0").is_none());
        assert_eq!(
            format!("{:?}", Token::new(" secret ").expect("a token")),
            "Token(<redacted>)"
        );
        assert!(Token::new("  ").is_none());
    }
}
