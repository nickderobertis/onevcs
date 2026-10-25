//! The rows of the private findings report, and the false-positive survey kept
//! beside them.
//!
//! Rows are split by how an exposure can be undone, because that is the decision the
//! report exists for: a current file is one commit away from gone, history needs a
//! rewrite, and an issue's old text survives an edit wherever the host shows edit
//! history. Every row names its term, the term's class and match mode, and the
//! private identities it was derived from.

use std::collections::{BTreeMap, BTreeSet};

use serde::Serialize;

use crate::terms::{Fields, Hit, Terms};

/// Where in a file at the tip a term was found.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum FileLocation {
    Content,
    Path,
}

/// Where in reachable history a term was found.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistoryLocation {
    /// A file's content in some commit.
    Blob,
    /// A path some tree held.
    Path,
    /// A commit message.
    Message,
    /// A commit's author or committer identity: a name and an email.
    Identity,
    /// A ref's name.
    Ref,
    /// An annotated tag's message.
    Tag,
}

/// What kind of host item a term was found in.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ItemKind {
    Issue,
    IssueComment,
    ChangeRequest,
    ChangeRequestComment,
    Review,
    ReviewComment,
    Board,
    BoardField,
    DraftItem,
    BoardIssue,
    BoardIssueComment,
    BoardChangeRequest,
    BoardChangeRequestComment,
}

/// Whether an item's text reads that way now, or survives only where the host
/// shows earlier revisions — which an edit does not undo.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum Persistence {
    /// The text as it reads now.
    Current,
    /// An earlier revision the host still shows.
    EditHistory,
    /// An earlier revision since deleted from the host's edit history, read from
    /// what the host still returns for it.
    DeletedEditHistory,
    /// A title the item was renamed from, kept in its timeline.
    TitleHistory,
}

/// An issue's or change request's state, as the host names it. A state the host
/// adds later is not guessed at: the row carries none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum ItemState {
    Open,
    Closed,
    Merged,
}

impl ItemState {
    pub fn parse(state: &str) -> Option<ItemState> {
        match state {
            "OPEN" => Some(ItemState::Open),
            "CLOSED" => Some(ItemState::Closed),
            "MERGED" => Some(ItemState::Merged),
            _ => None,
        }
    }
}

/// A term in a file at the default branch's tip.
#[derive(Serialize)]
pub struct FileRow<'a> {
    pub repository: &'a str,
    pub path: &'a str,
    pub location: FileLocation,
    #[serde(flatten)]
    pub term: Fields<'a>,
    pub line: Option<usize>,
    pub snippet: String,
}

/// A term in reachable history.
#[derive(Serialize)]
pub struct HistoryRow<'a> {
    pub repository: &'a str,
    pub commit: &'a str,
    #[serde(flatten)]
    pub term: Fields<'a>,
    pub location: HistoryLocation,
    pub path: Option<&'a str>,
    pub snippet: String,
}

/// Why a history hit's commits are not all listed.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum HistoryGap {
    /// More commits carry it than a hit is attributed to.
    AttributionTruncated,
    /// No commit carries it: only a ref naming a tree or a blob reaches it.
    Unattributed,
}

/// A history hit whose attribution is incomplete. It carries the hit itself, so a
/// hit no commit carries is kept rather than dropped with its missing rows.
#[derive(Serialize)]
pub struct HistoryGapRow<'a> {
    pub repository: &'a str,
    pub gap: HistoryGap,
    pub location: HistoryLocation,
    /// The blob's id, or the path.
    pub object: &'a str,
    /// For a blob, a path it is at, where one is known.
    pub path: Option<&'a str>,
    /// A ref that reaches it and is not a commit.
    #[serde(rename = "ref")]
    pub reached_by: Option<&'a str>,
    pub terms: Vec<&'a str>,
    pub snippets: Vec<&'a str>,
    /// The commits its history rows list.
    pub attributed: u64,
    /// The commits that carry it.
    pub commits: u64,
}

/// A term in an issue, a change request, a comment, a review, or a board item.
#[derive(Serialize)]
pub struct ItemRow<'a> {
    /// `owner/name`, or `board:OWNER/NUMBER`.
    pub container: &'a str,
    pub kind: ItemKind,
    pub number: Option<u64>,
    pub url: Option<&'a str>,
    pub state: Option<ItemState>,
    #[serde(flatten)]
    pub term: Fields<'a>,
    pub persistence: Persistence,
    pub snippet: String,
}

/// How often each term class hits, and how often a hit stands alone — no
/// `owner/name` hit in the same text — which is what a false positive looks like
/// from outside; and where a host public phrase exempted an occurrence instead.
#[derive(Clone, Debug, Default, Serialize)]
pub struct Survey {
    pub by_class: BTreeMap<&'static str, ClassTally>,
    /// Every text a host public phrase exempted an occurrence in. Private: it is
    /// where a phrase is, and a phrase carries a term.
    pub exempted: Vec<Exempted>,
}

/// Where host public phrases exempted occurrences of a term, and how many.
#[derive(Clone, Debug, Serialize)]
pub struct Exempted {
    // llmlint: ignore[invalid_states_unrepresentable] the container is the label every
    // row of this crate carries (`ItemRow::container`, `HistoryRow::repository`), formed
    // once per repository or board by the run loop from its `RepoId` or `BoardId` and
    // handed to every scan site as text; a second, typed spelling here would be the one
    // row that could disagree with the findings beside it.
    /// `owner/name`, or `board:OWNER/NUMBER`.
    pub container: String,
    /// Which text it was, with the identifier that names it.
    pub location: ExemptLocation,
    pub occurrences: u64,
}

/// The text an exempted occurrence was in, each kind carrying the identifier the
/// findings rows name that kind of text by.
// llmlint: ignore[invalid_states_unrepresentable] each identifier is what git or the host
// printed for that text — an object id off `git log` or `cat-file`, a ref or path git
// listed, an item URL from the host's own answer — copied through to the vault exactly as
// `HistoryRow::commit`, `FileRow::path` and `ItemRow::url` carry the same values beside
// it; a newtype would re-validate text git and the host wrote, for a report nothing parses.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ExemptLocation {
    /// A blob's content, by the blob's id.
    Blob(String),
    /// A path some tree held.
    Path(String),
    /// A commit's message, by the commit's id.
    Message(String),
    /// A commit's author or committer identity, by the commit's id.
    Identity(String),
    /// A ref's name.
    Ref(String),
    /// An annotated tag's message, by the tag's id.
    Tag(String),
    /// An issue, change request, comment, review or board item, as it reads now or
    /// as a revision the host still shows, by its URL or number where the host gave one.
    Item {
        kind: ItemKind,
        persistence: Persistence,
        url: Option<String>,
        number: Option<u64>,
    },
}

impl ExemptLocation {
    /// A commit's text at `location`, which is its message or one of its identities.
    pub fn of_commit(location: HistoryLocation, commit: &str) -> ExemptLocation {
        match location {
            HistoryLocation::Message => ExemptLocation::Message(commit.to_owned()),
            _ => ExemptLocation::Identity(commit.to_owned()),
        }
    }

    /// How the private report names it: what kind of text, and which — `None` for an
    /// item the host gave neither a URL nor a number, such as a board's field value or
    /// a draft.
    pub fn label(&self) -> (String, Option<String>) {
        let history = |location: HistoryLocation, at: &str| (word(&location), Some(at.to_owned()));
        match self {
            ExemptLocation::Blob(at) => history(HistoryLocation::Blob, at),
            ExemptLocation::Path(at) => history(HistoryLocation::Path, at),
            ExemptLocation::Message(at) => history(HistoryLocation::Message, at),
            ExemptLocation::Identity(at) => history(HistoryLocation::Identity, at),
            ExemptLocation::Ref(at) => history(HistoryLocation::Ref, at),
            ExemptLocation::Tag(at) => history(HistoryLocation::Tag, at),
            ExemptLocation::Item {
                kind,
                persistence,
                url,
                number,
            } => (
                format!("{} {}", word(kind), word(persistence)),
                url.clone().or_else(|| number.map(|n| format!("#{n}"))),
            ),
        }
    }
}

/// A row field's wire word, as the findings files spell it.
fn word(value: &impl Serialize) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|v| v.as_str().map(str::to_owned))
        .unwrap_or_default()
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct ClassTally {
    pub hits: u64,
    pub uncorroborated: u64,
    /// The texts with at least one hit of this class.
    pub texts: u64,
    /// The repositories or boards those texts are in.
    pub containers: BTreeSet<String>,
}

impl ClassTally {
    /// The share of hits that stand alone, in whole percent — but only once the hits
    /// come from more than one container and more than one text, since a proportion
    /// over a single source can still point at it.
    pub fn public_percent(&self) -> Option<u64> {
        (self.containers.len() > 1 && self.texts > 1 && self.hits > 0)
            .then(|| (self.uncorroborated * 100 + self.hits / 2) / self.hits)
    }
}

impl Survey {
    /// Tally the hits of one text found in `container`.
    pub fn tally(&mut self, terms: &Terms, hits: &[Hit], container: &str) {
        let corroborated = hits.iter().any(|h| terms.qualified(h.rule));
        let mut touched: BTreeSet<&'static str> = BTreeSet::new();
        for hit in hits {
            let class = terms.class(hit.rule).as_str();
            let tally = self.by_class.entry(class).or_default();
            tally.hits += 1;
            if !corroborated {
                tally.uncorroborated += 1;
            }
            touched.insert(class);
        }
        for class in touched {
            let tally = self.by_class.entry(class).or_default();
            tally.texts += 1;
            tally.containers.insert(container.to_owned());
        }
    }

    /// Record what a host public phrase exempted in `text`, found in `container` at
    /// what `location` names. The matcher decides; this only keeps where, and names
    /// it only where something was exempted.
    pub fn exempt(
        &mut self,
        terms: &Terms,
        text: &str,
        container: &str,
        location: impl FnOnce() -> ExemptLocation,
    ) {
        let occurrences = terms.exempted(text);
        if occurrences > 0 {
            self.exempted.push(Exempted {
                container: container.to_owned(),
                location: location(),
                occurrences,
            });
        }
    }

    /// Every occurrence a host public phrase exempted.
    pub fn exempted_total(&self) -> u64 {
        self.exempted.iter().map(|e| e.occurrences).sum()
    }

    /// What the public measurements may say: a whole percent per class where
    /// [`ClassTally::public_percent`] allows one, and nothing else.
    pub fn public(&self) -> serde_json::Value {
        let percents: serde_json::Map<String, serde_json::Value> = self
            .by_class
            .iter()
            .filter_map(|(name, tally)| {
                tally
                    .public_percent()
                    .map(|p| ((*name).to_owned(), p.into()))
            })
            .collect();
        serde_json::json!({ "uncorroborated_percent_by_class": percents })
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use onevcs::boundary::{PublicNames, TermSource};

    use super::*;

    #[test]
    fn a_proportion_over_a_single_source_is_withheld() {
        let source = TermSource {
            identity: "github.com/hiddenco/quietharbor".into(),
            owner: Some("hiddenco".into()),
            name: "quietharbor".into(),
            packages: BTreeSet::new(),
            declaration: None,
        };
        let (terms, _) = Terms::build(
            &[source],
            &PublicNames::default(),
            &onevcs::boundary::PublicPhrases::default(),
        );
        let terms = terms.expect("a small set compiles");
        let mut survey = Survey::default();
        for text in ["quietharbor alone", "hiddenco/quietharbor"] {
            survey.tally(&terms, &terms.find(text), "sample/one");
        }
        assert_eq!(
            survey.public()["uncorroborated_percent_by_class"],
            serde_json::json!({})
        );
        survey.tally(&terms, &terms.find("quietharbor again"), "sample/two");
        // Three bare-name hits, two of them with no `owner/name` beside them.
        assert_eq!(
            survey.public()["uncorroborated_percent_by_class"]["name"],
            67
        );
    }
}
