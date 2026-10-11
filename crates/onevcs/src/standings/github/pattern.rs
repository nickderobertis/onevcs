//! GitHub's branch patterns, read the way GitHub reads them: Ruby's `File.fnmatch` with
//! `FNM_PATHNAME`, which classic protection rules and rulesets' ref conditions both use.
//!
//! `*` is any run of characters within one `/`-separated segment, `?` one character that
//! is not `/`, `[...]` one character of a class (ranges included, `[!...]` or `[^...]`
//! negating it) that is not `/`, `**/` zero or more whole segments, any other `**` any run
//! of characters crossing `/`, and anything else itself.
//!
//! **A pattern this cannot evaluate says so, and is never guessed at.** A brace (which
//! GitHub may or may not expand), an escape, and a class with no closing `]` or nothing in
//! it each make [`Pattern::parse`] answer why, so the caller calls what that rule requires
//! unknown — reading such a rule as matching nothing would let a required check with no
//! run read as nothing being required.

/// One piece of a parsed pattern.
enum Token {
    Literal(char),
    /// `?`.
    One,
    /// `*`.
    Segment,
    /// `**` not followed by `/`.
    Anything,
    /// `**/`.
    Segments,
    /// `[...]`: its ranges, and whether it is negated.
    Class {
        ranges: Vec<(char, char)>,
        negated: bool,
    },
}

/// A branch pattern GitHub's grammar spells, parsed.
pub(super) struct Pattern {
    tokens: Vec<Token>,
}

impl Pattern {
    /// `spelled` parsed, or why it cannot be evaluated.
    pub(super) fn parse(spelled: &str) -> std::result::Result<Self, String> {
        let unevaluable = |why: &str| {
            Err(format!(
                "the branch pattern {spelled:?} {why}, which this build cannot evaluate"
            ))
        };
        let chars: Vec<char> = spelled.chars().collect();
        let mut tokens = Vec::new();
        let mut at = 0;
        while at < chars.len() {
            let token = match (chars[at], chars.get(at + 1), chars.get(at + 2)) {
                ('*', Some('*'), Some('/')) => (Token::Segments, 3),
                ('*', Some('*'), _) => (Token::Anything, 2),
                ('*', ..) => (Token::Segment, 1),
                ('?', ..) => (Token::One, 1),
                ('{' | '}', ..) => return unevaluable("holds a brace"),
                ('\\', ..) => return unevaluable("holds an escape"),
                ('[', ..) => {
                    let negated = matches!(chars.get(at + 1), Some('!' | '^'));
                    let open = at + 1 + usize::from(negated);
                    let Some(close) = chars[open..].iter().position(|c| *c == ']') else {
                        return unevaluable("opens a class it never closes");
                    };
                    let inside = &chars[open..open + close];
                    if inside.is_empty() {
                        return unevaluable("holds an empty class");
                    }
                    let mut ranges = Vec::new();
                    let mut from = 0;
                    while from < inside.len() {
                        match (inside.get(from + 1), inside.get(from + 2)) {
                            (Some('-'), Some(&high)) => {
                                ranges.push((inside[from], high));
                                from += 3;
                            }
                            _ => {
                                ranges.push((inside[from], inside[from]));
                                from += 1;
                            }
                        }
                    }
                    (Token::Class { ranges, negated }, open + close + 1 - at)
                }
                (literal, ..) => (Token::Literal(literal), 1),
            };
            tokens.push(token.0);
            at += token.1;
        }
        Ok(Pattern { tokens })
    }

    /// Whether this pattern has no wildcard — which is what GitHub ranks over every
    /// pattern matching the same branch.
    pub(super) fn exact(&self) -> bool {
        self.tokens
            .iter()
            .all(|token| matches!(token, Token::Literal(_)))
    }

    /// Whether `name` matches this pattern.
    pub(super) fn matches(&self, name: &str) -> bool {
        let name: Vec<char> = name.chars().collect();
        matches(&self.tokens, &name)
    }
}

fn matches(tokens: &[Token], name: &[char]) -> bool {
    let one = |fits: &dyn Fn(char) -> bool, rest: &[Token]| {
        name.split_first()
            .is_some_and(|(first, tail)| *first != '/' && fits(*first) && matches(rest, tail))
    };
    match tokens {
        [] => name.is_empty(),
        [Token::Literal(literal), rest @ ..] => name
            .split_first()
            .is_some_and(|(first, tail)| first == literal && matches(rest, tail)),
        [Token::One, rest @ ..] => one(&|_| true, rest),
        [Token::Class { ranges, negated }, rest @ ..] => one(
            &|have| {
                ranges
                    .iter()
                    .any(|(low, high)| (*low..=*high).contains(&have))
                    != *negated
            },
            rest,
        ),
        [Token::Segment, rest @ ..] => (0..=name.len())
            .take_while(|at| *at == 0 || name[at - 1] != '/')
            .any(|at| matches(rest, &name[at..])),
        [Token::Anything, rest @ ..] => (0..=name.len()).any(|at| matches(rest, &name[at..])),
        [Token::Segments, rest @ ..] => {
            matches(rest, name)
                || (0..name.len())
                    .filter(|at| name[*at] == '/')
                    .any(|at| matches(rest, &name[at + 1..]))
        }
    }
}
