# The onevcs contract

<!--
Everything below the horizontal rule is the approved contract, committed
verbatim. It is the source of truth for the crate's public surface, and the
suite reads its fixtures out of this file — the JSON envelope, the rules YAML,
the Rust declarations, the CLI usage, and the event-kind list are all extracted
and asserted against the code, so editing one without the other fails the gate.

Do not reword, reformat, or "fix" the text below. A conflict inside it is
reported, not resolved by editing.

Scope note: the task that approved this also carried a packaging section naming a
host-local reference checkout. That section governs how this repository is built
and released rather than what the crate exposes, so it is not reproduced here.
-->

## Amendments, recorded outside the approved text

The contract below is committed verbatim and is never edited. An approved
extension to it is written here instead, and the suite reconciles it with the code
the same way it reconciles the text below.

**The provenance trailer prefix is configurable, and the rules file is at
version 2.** The contract requires `Recovered-Incomplete` trailers and an
incomplete-step commit but fixes no keys, so this crate spells them
`<prefix>Status: incomplete`, `<prefix>Change-Base:`,
`<prefix>Recovered-Incomplete:`, `<prefix>Change-Url:`, and
`<prefix>Landed-Commit:`.

The last records a landing, and it is written on whichever side of one would
otherwise keep no record. A `local-direct` publication squashes a branch onto its
base and opens no change request for anything to find later, so the commit it lands
carries that key on the *base*, with the branch commit whose work it landed as its
value. A change the host lands on its own clock is the mirror of that: the host
writes the change request's own number onto the base, and nothing at all onto the
branch — so the key is written on the *branch* instead, carrying the commit the
change reached its base at (below). One key for both, because both answer the one
question, and a reader tells them apart by which side the commit sits on. A commit
rather than a branch name, because a branch name is spent and re-cut and a landing
of the work that used to wear it must not answer for work that wears it now.

The prefix is the rules file's optional `trailer_prefix` key, unset `Onevcs-`.

Version 2 is that key and nothing else, so the version below is the whole schema
otherwise. A `version: 1` file still loads and still means the default prefix —
there is no field to migrate, only one that is absent — and a `version: 1` file
that *names* a `trailer_prefix` is refused rather than obeyed or ignored: a file
whose key is not in the version it declares reads one way here and another wherever
that version is trusted, and for this key those two readings are provenance written
under one prefix and searched for under another. Version 2 in full:

```yaml
version: 2
trailer_prefix: Onevcs-
rules:
  - match: {host: github.com, owner: acme-corp, name: "*"}
    publication: change-open          # local-direct | change-open | change-auto | change-direct
    approvals: required               # required | none
    gate: {kind: checks}              # checks | pre-push | command: [...]
  - match: {path: "~/projects/*"}
    publication: local-direct
    gate: {kind: pre-push}
default: {publication: change-open, approvals: required, gate: {kind: checks}}
```

`trailer_prefix` is omitted when it is unset, so a version 2 file that configures
nothing is byte-for-byte the version 1 file the contract spells below, with one
number changed.

That default is the value this crate has always written, so a host that configures
nothing sees no change. One prefix is used for writing *and* reading, so a branch
preserved under a prefix is recognized, listed by `recoverable`, and published by
`recover` under that same prefix. A prefix that could not spell a git trailer key
— anything outside letters, digits and `-`, one that does not start with a letter
or a digit, or an empty one — is refused when the rules file is loaded, naming what
was wrong. The key's type is `rules::TrailerPrefix`, which is the one public item
this amendment adds: the check is in its conversion, so an unusable prefix does not
deserialize rather than being caught somewhere later.

A branch whose incomplete marker is written under a prefix this host is *not*
configured with is reported and refused rather than read as complete: `recoverable`
lists it as an incomplete step naming the prefix it found, `integrate` skips it, and
`recover` refuses it and says which prefix to configure. Nothing here knows any
particular prefix — the shape recognized is the marker's own, under whatever prefix
it carries.

**Publishing, closing a session, and reading its events are library calls, and the
session record is behind the seam.** The contract declares the two interfaces "with
a trait seam" and gives a run one answer: a process exit code. A caller embedding
this crate has to branch on what a publication *did*, and neither a `u8` nor the
prose printed beside it carries that — so the first consumer parsed the prose,
wrongly, and shipped it. The root cause is one thing: the session record was
written by `Git` directly rather than through the interface, so a session a
supplied implementation opened was refused by every command that took one.

`Vcs` therefore gains three methods, and each of the operations they serve gains a
typed entry point beside the CLI. The CLI is unchanged — same arguments, same
output, same exit codes — and `run` and `run_with` keep their signatures.

```rust
pub trait Vcs {                              // the five above, unchanged, plus:
    fn session(&self, token: &SessionToken) -> Result<SessionRecord>;
    fn close_session(&self, token: &SessionToken) -> Result<Session>;
    fn publish(&self, token: &SessionToken, req: &PublishRequest, hosting: &dyn Hosting)
        -> Result<Publication>;
}
pub fn publish(p: &Providers, token: &SessionToken, req: &PublishRequest) -> Result<Publication>;
pub fn close_session(p: &Providers, token: &SessionToken) -> Result<Session>;
pub fn session(p: &Providers, token: &SessionToken) -> Result<SessionRecord>;

pub struct SessionRecord { pub session: Session, pub identity: String,
                           pub lifecycle: Lifecycle, pub provenance: Provenance }
pub enum Lifecycle { Open, Closed }
pub struct PublishRequest { pub policy: Option<MergePolicy>, pub title: Option<Subject>,
                            pub body: Option<String> }   // widened by the body amendment
pub struct Subject(String);                  // TryFrom<String>: a title that can be one
pub struct Publication { pub session: SessionToken, pub branch: String,
                         pub policy: MergePolicy, pub outcome: PublishOutcome }
pub enum PublishOutcome {
    Merged(Sha), ChangeOpen(Url), Queued(Url), NothingToPublish,
    Failed { kind: FailureKind, reason: String, retained: Option<Retention> },
}
pub enum FailureKind { Gate, Invalid, SyncConflict, NotImplemented }  // 1 | 2 | 3 | 70
pub enum Retention { HandedBack(PathBuf), Refused(PathBuf) }

impl EventStream {                           // `onevcs events TOKEN`, as values
    pub fn open(session: &SessionToken) -> Result<Self>;
    pub fn session(&self) -> &SessionToken;
    pub fn read(&mut self) -> Result<Vec<Envelope>>;
}
```

`publish` takes the host side explicitly because a publication is the one operation
that reaches both interfaces: the repository side lands it, and the host side opens
and merges the change request. `Providers` still bundles the two, which is what the
free function above takes.

A publication that did not land is a `Failed` outcome rather than an `Err`, exactly
where the CLI reported a non-zero exit rather than a refusal — so the two surfaces
cannot disagree about which failures are which. The exit codes the contract fixes
are unchanged and are now stated once, on `FailureKind::exit_code`, beside
`FailureKind::of(&Error)`, which is how any implementation answers with the kind
this one would.

Three rules that belong to publication rather than to any one implementation of it
are public for the same reason, so a supplied `Vcs` applies the rule rather than a
restatement that could accept what the real one refuses: `MergePolicy::narrow` (a
per-run policy may narrow the repository's and never widen it), `FailureKind::of`,
and `Subject`, which is the type of an explicit title. That one is a *type* rather
than a method because of where a publication would otherwise meet it: it commits
the session's work and merges its base before it composes a message, so a title
refused where the message is composed is refused after a commit nobody can undo.
The check is in the conversion, as it is for every other validated name here, so a
`PublishRequest` carrying a title that could not be a subject is unrepresentable.

**A change request's checks say which of the host's sources they were read from.**
The contract has `change_checks` answer `Vec<Check>`, and this crate read that list
out of `gh pr view --json statusCheckRollup` and `gh pr checks`. **A fine-grained
personal access token cannot read either, on any repository, with any permission**:
GitHub offers no `Checks` permission for that credential class — it existed briefly,
was withdrawn, and has not returned — so both commands answer `Resource not
accessible by personal access token`, and the two methods a lifecycle decides a
merge by were unusable for the credential GitHub steers people toward. The Actions
API is what such a token *can* read, with `Actions: Read`, and a workflow job is the
same unit GitHub posts a check run for.

Reading a narrower source is not free, and the loss is in the answer rather than in
a comment: a check that ran no workflow — anything a third-party integration posted
as a check run or a commit status — is invisible to that credential, and a caller
deciding whether a change may merge has to be able to tell that from having seen
everything. So the return says where it looked.

```rust
pub trait RemoteHost {                       // the six above, one of them widened:
    fn change_checks(&self, cr: &ChangeRequest) -> Result<ChangeChecks>;
}
pub struct ChangeChecks { pub checks: Vec<Check>, pub sources: BTreeSet<CheckSource> }
impl ChangeChecks { pub fn complete(&self) -> bool; }        // sources holds StatusChecks
pub enum CheckSource { StatusChecks, Actions, BranchRules }  // status-checks|actions|branch-rules
```

`Check` is unchanged, and so is every other method. `sources` is never empty: a host
whose sources could none of them be read is an `Err` naming each refusal and the
permission that answers one of them, never an empty list — "could not look" reported
as "nothing blocks this" is the one answer that turns an ungated merge into one that
looks gated.

The three sources: `StatusChecks` is the host's own rollup, which is every check on
the change request and carries whether each blocks; `Actions` is the jobs of the
workflow runs on the head commit; `BranchRules` is the repository's rulesets, read
for which checks block a merge into the base when the rollup cannot be. `BranchRules`
is narrower than the rollup's own answer — it reports rulesets and not classic branch
protection — so a repository protected the classic way reports nothing blocking, and
the publication path fails closed on that, waiting for a required check that never
arrives rather than merging without one.

`GitHub` consults the rollup first and falls back to Actions **only** where the
credential may never read the rollup; a rollup that came back garbled, or a host that
would not say which of its checks block, is still a refusal, because answering from
Actions alone whenever the complete answer went wrong would silently drop whatever a
third-party integration posted. `ONEVCS_CHECK_SOURCE` (`auto`, `status-checks`,
`actions`) narrows that to one source for an operator who already knows what their
credential can read; an unrecognized value is refused where it is named.

**Enumerating a repository's session holders is a library call too.** The contract
gives the enumeration one surface — `onevcs session holders REPO [--json]` — and a
caller embedding this crate had no route to it at all: the records, the view over
them, and the reader were each private, so the only way to ask which sessions hold a
repository's workspaces was to spawn the binary and parse what it printed. That is
the same defect the amendment above closed for publishing, met one question earlier:
before a caller has a token to publish or close, it has to find out who is here.

```rust
pub fn session_holders(repo: &str) -> Result<Vec<SessionHolder>>;

pub struct SessionHolder { pub token: SessionToken, pub identity: String,
                           pub branch: String, pub worktree: PathBuf,
                           pub owner_pid: u32, pub state: Lifecycle,
                           pub liveness: Liveness }
pub enum Liveness { Live, Stale }            // live|stale
impl Liveness { pub fn as_str(&self) -> &'static str; }
```

The CLI is a rendering of that call and no longer a second reader of the store, so
its output — the JSON keys, the human line, the exit codes, the refusal of a
repository nothing resolves — is unchanged. `state` rather than `lifecycle` is the
field name for exactly that reason: it is the key the command has always printed,
and the Rust field and the JSON one stay the same word.

`liveness` is reported rather than derived, because a caller cannot derive it: a pid
alone is not an owner, since the OS reuses pids and a later process wearing a dead
session's number would read as live. The answer is that pid *and* the recorded
creation identity of the process behind it, which only the reader of the record has.

It takes no `Providers`, and that is the boundary of what this amendment claims. The
holders are the records under this host's state root — the thing `Git` writes and
the command reads — so there is nothing here for a supplied implementation to
answer, and a `Vcs` that keeps its sessions elsewhere does not appear in the list.
That is the command's own limit, unchanged; routing enumeration through the seam
would add a required method to a trait consumers implement, and is the next
question rather than this one.

<!-- llmlint: ignore[contracts_have_one_source_or_a_drift_gate] the duplication is the
approved contract's own mechanism rather than a missing gate, and this amendment cannot
close it from inside one of the three repositories. The envelope section below the rule
already fixes it: those types are duplicated per crate with deliberately no shared util
crate, and "a cross-repo contract test asserts this crate's envelope serialization
against the spec fixtures committed in docs/contract.md". The filter follows that
pattern, as the paragraph below says. So the authoritative artifact for *this* copy is
the fixture below, which `a_filter_round_trips_through_the_grammar_and_writes_only_what_was_set`
and `the_amendment_declares_the_filter_a_stream_is_read_through` in tests/contract.rs
hold the code to — extracted, never restated — and the cross-repository half is that same
committed fixture read by the contract owner's test. A shared artifact here would be
exactly the shared source the contract refuses. -->

**Reading a session's events takes a filter, and it is the grammar the three
producing libraries share.** The contract gives both surfaces one answer: every
event the session wrote. Consumers read the same stream under different attention
budgets — a monitor wants the activity, a planner wants the decisions and the
settlements — and the composition layer (`onepipeline`) that narrowed it for them
would be re-implementing, once per source, the only thing a source can do that a
consumer cannot: not send the event at all. So filtering is owned by each stream
source, and the grammar is the approved one, identical in `onevcs`,
`oneagentgraph`, and `onepipeline`:

```yaml
include:            # list of matchers; absent or empty = everything passes include
  - {source: vcs, kind: "gate-*"}
exclude:            # list of matchers; a match here always rejects (wins over include)
  - {kind: lock-wait}
```

The three copies are held together the way the envelope's already are: each
repository commits this fixture in its own contract, its own suite holds its own
code to its own copy, and the cross-repo contract test reads those committed
fixtures. There is nothing here to import and nothing to generate from — that is
the point of "duplicate these types; there is deliberately no shared util crate" —
so a departure from the grammar is raised with the contract owner as a proposal,
never taken here.

An envelope passes when it matches any `include` matcher — or `include` is absent
or empty — and matches no `exclude` matcher. Matcher fields are all optional and
conjoin within one matcher: `source` is the envelope's source family, by exact
equality; `kind` is a glob over the event kind's kebab-case wire string, so
`change-*` is `change-opened`, `change-check`, and `change-merged`; and `run_id`,
`node`, `step`, `member`, and `persona` are exact equality against the envelope's
reserved `labels` keys, where a matcher naming a label the envelope did not stamp
does not match it. Deliberately **not** in the grammar: `stream`, which is a
producing process's id rather than a family, and payload fields, which differ per
kind. The envelope types are duplicated per repository by design, held together by
a cross-repo contract test rather than by a shared util crate; the filter type
follows the same pattern.

```rust
impl EventStream {                           // `open`, `session`, `read` unchanged, plus:
    pub fn open_filtered(session: &SessionToken, filter: EventFilter) -> Result<Self>;
}
pub struct EventFilter { pub include: Vec<EventMatcher>, pub exclude: Vec<EventMatcher> }
pub struct EventMatcher { pub source: Option<Source>, pub kind: Option<String>,
                          pub run_id: Option<String>, pub node: Option<String>,
                          pub step: Option<String>, pub member: Option<String>,
                          pub persona: Option<String> }
impl EventFilter {
    pub fn parse(spec: &str) -> Result<Self>;            // the JSON or YAML above
    pub fn matches(&self, envelope: &Envelope) -> bool;
}
```

The command gains the same filter as an argument: `onevcs events TOKEN [--follow]
[--filter SPEC]`, where SPEC is the spec inline as JSON when it opens with `{` and
the path of a file holding one otherwise — decided by the text, so what an
invocation means does not change with the directory it is run from. It applies
identically to a followed read and a one-shot one, and the events it admits are
printed as the bytes the producer wrote rather than as a re-serialization, so a
filtered stream is a subset of the unfiltered one byte for byte.

Additive throughout: `open` is `open_filtered` with `EventFilter::default()`, which
admits everything, and a command with no `--filter` reads exactly what it read
before. A spec that names a field the grammar does not have, a matcher that is not
a mapping, or an `include`/`exclude` that is not a list is refused where the spec is
read, naming the matcher — never read leniently as match-nothing or
match-everything, for the reason the rules loader refuses an unusable bound: both
of those are answers a consumer acts on without ever being told it asked for
something else. Under `--filter` the command reads every line as a value, and is
therefore held to what a reader of values is held to: a line this build cannot
parse, and a line carrying an event of another stream, are both refused where they
are read, naming the line — the same two refusals `EventStream::read` gives,
through the same seam. Unfiltered the command reads nothing and prints the line as
the file's own bytes, which is unchanged. The reason the filtered path cannot keep
that posture is what a filter would otherwise do with such a line: judge one
session's event against a statement its consumer made about another, and then
report it as this session's or drop it in silence. Both are answers about an event
nobody read.

**The grammar carries no version, and this crate does not give it one.** Every
other serialized shape here is versioned — the envelope at `v: 1`, the rules file
at 2, the registry at 5 — and each of those is written and read by this repository
alone, so this repository can bump it. A filter is not: it is written by whoever
configures a run and read by all three libraries, so a `version` key one of them
writes and the others refuse is the shared grammar ceasing to be shared, and the
failure lands on the consumer that did nothing wrong. Adding one here would be
exactly the unilateral change the grammar is fixed against.

So the constraint is stated rather than closed, and what makes an unversioned
document safe is the refusal above: an unknown top-level key — `version` included —
is refused rather than ignored, so a spec written for a grammar this build does not
have fails closed at the boundary instead of being half-read as a narrower filter
than its author wrote. `a_filter_round_trips_through_the_grammar_and_writes_only_what_was_set`
in `tests/contract.rs` holds both halves: nothing this crate writes carries a
version, and a document that names one is refused. Versioning the grammar is a
**proposal to raise with the contract owner across the three repositories** — it
needs one spelling, one meaning for an absent version, and one answer for what an
older build does with a newer document — and until then, the compatible way to
extend a filter is a new matcher field agreed in the same place, which an older
build already refuses rather than misreads.

Two things this crate cannot compile exactly as the grammar is written, recorded
here rather than resolved:

- **`onevcs events` has one output mode, so "both output modes" is one.** The
  command renders NDJSON and nothing else — this crate has no text rendering of an
  event stream to keep in step, unlike `session holders` and `recoverable`, which
  are `--json`-or-human. The filter therefore applies to the one rendering there
  is; a text rendering added later renders the same filtered events, which is the
  shared envelope contract's own rule.
- **`onevcs` stamps none of the reserved label keys today.** It stamps `session`
  and `identity`, which are free-form extras, so a `run_id`, `node`, `step`,
  `member`, or `persona` matcher admits no envelope this crate currently produces —
  correctly, by the grammar's own "a matcher naming a label the envelope did not
  stamp does not match it". The keys are in the matcher because the envelope is
  shared and enrichers stamp them; making `onevcs` stamp them is a question about
  what a session knows of the run around it, not about filtering, and is not
  answered here.

**A change request opens with the body its caller passed, or with none at all.**
The contract gives `ChangeSpec` a `body` and says nothing about where one comes
from, so this crate composed one: every change request it opened carried the
branch's own subject echoed back under `## What`, `Published by onevcs.` under
`## Why`, and an `## Additional info` section holding the publication's provenance
trailers. A reviewer who opened one learned nothing the title had not already told
them, and the layer that does know what the change is *for* — the agent or the
pipeline that asked for the publication — had no way to say it. This crate is the
layer that decides what body a change request gets, so it is the layer that accepts
one.

`PublishRequest` therefore carries the body, declared with the amendment above, and
a publication opens its change request with exactly that text or with no body
whatsoever. Nothing is composed, nothing is appended, and an absent body is absent
rather than empty scaffolding. The provenance trailers do not move: they are the
publication *commit*'s, composed by `compose_message`, and a recovered incomplete
step still records `Recovered-Incomplete:` there — they simply stop appearing in a
change request's body, which was never a record anything read them back out of.

**A publication observes every surface it can fail at, says which failure it was,
and does not settle while the host still has the change.** The contract gives
`publish` one code for a verification failure and one line of prose beside it. Both
were doing too much work. A publication can fail at four surfaces — the gate, the
publishing push, the sync with its base, and the host's required checks — and three
of them threw their evidence away: a rejected push preserved what the hook wrote
only where the resolved policy named `gate: {kind: pre-push}`, which on a host whose
every rule names a `command:` gate is no repository at all; a sync conflict reported
that something conflicted and never what; and a red required check named the check
and left the reason reachable only by fetching an artifact by hand. Worse, under
`change-auto` the publication *ended* at change-request-open: the host landed the
change later on its own clock, and no node remained alive to report that it had, or
that a check had stopped it.

Evidence travels on the events, and none of it is conditional:

- `push` carries what the push wrote — git's own porcelain and whatever the
  repository's `pre-push` hook printed — as a `log` artifact, with `output` and
  `preserved_log` beside it, for **every** publishing push, accepted or rejected,
  whatever the policy names as its gate. Where the policy does name `pre-push`, the
  same artifact is referenced again from that gate's `gate-verdict`; one run of a
  gate is one artifact.
- `sync-conflict` carries `paths`, the paths git left unmerged, and the hunks it
  renders for them as a `diff` artifact. Both are read out of the tree git had
  already opened, before the attempt is aborted, so neither costs a second merge.
  Every refusal about a conflict names those paths.
- `change-check` already carried the failing check's log; the refusal about it now
  quotes a bounded excerpt of that log beside the check's name, so the diagnosis is
  in the failure and not only behind an artifact id.

Which failure it was is a case rather than a sentence:

```rust
pub enum FailureKind { Gate, Invalid, SyncConflict, NotImplemented,
                       ChecksFailed, ChecksUnsettled, PushRejected }  // 1 | 2 | 3 | 70 | 1 | 1 | 1
```

`ChecksFailed` is a required check that concluded red, and its `reason` names the
check. `ChecksUnsettled` is the bound elapsing with required checks unsettled, and
its `reason` names the checks still pending — the bound had been a second silent
failure. `PushRejected` is a push the merge path refused, and its `reason` carries
git's own per-ref refusal. All three keep **exit code 1**, which is the code the
contract already fixes for a verification failure: what is new is which verification
it was, and only a caller that branches on the kind can see it. `Error` gains the
matching variants (`ChecksFailed`, `ChecksUnsettled`, `PushRejected`); it is
`#[non_exhaustive]`, which is what makes that additive.

What a publication watches follows the **merge policy**, never what the policy names
as its verification:

- `change-auto` arms the host's own merge and then watches, until the host reports
  the merge — answering `Merged(sha)` with the commit read from the host's own
  answer — until a required check concludes red, or until the bound. It no longer
  answers `Queued` for a change the host is holding.
- `change-direct` asks for the merge itself, so it waits until nothing the host says
  blocks one is still unsettled. A host that declares *no* required check has
  answered, and its answer is that nothing blocks; the merge is then the host's own
  to refuse under its own rules. `change-auto` is where that fails closed, because
  its watch ends at a merge a host holding the change behind an undeclared check
  never performs.
- **`change-open` is the stated exception**: a human decides when a reviewed change
  merges, so there is no bounded wait to have and it settles at change-request-open
  exactly as before. That is a decision, not an oversight.

Watching needs one question the six methods could not ask, so `RemoteHost` gains a
seventh:

```rust
pub trait RemoteHost {                       // the six above, unchanged, plus:
    fn merged_at(&self, cr: &ChangeRequest) -> Result<Option<Sha>>;
}
```

`None` is "not yet" and never "somewhere else". It is **defaulted**, so an
implementation written against the earlier surface still compiles — and defaulted to
`Error::NotImplemented` rather than to `None`, because a host that was never taught
to answer has not said a change is unmerged, and a publication reading it that way
would watch to its bound and then report checks that were never the reason.

`ONEVCS_CHECKS_POLL_SECONDS` defaults to **30 seconds** rather than 5, against the
same hour-long `ONEVCS_CHECKS_TIMEOUT_SECONDS` bound. Each ask is a `gh` subprocess
and at least one API call, and the answer changes when a job finishes rather than
when it is asked about. The override is unchanged.

Finally, a merge the host reports is **recorded on the branch**, as one more
provenance trailer under the configured prefix: `<prefix>Landed-Commit:` carrying
the commit the change reached its base at, written the moment the host reports the
merge, on an otherwise empty commit — the branch's content is exactly what merged. It is read back through `trailer_prefix`
exactly as `<prefix>Recovered-Incomplete:` is, and it is what lets a later reader say
a branch landed because a landing was *recorded* rather than infer it from content
the base might have come by some other way. Writing it is best effort: the change has
already merged by then, and reporting the publication as failed because its own
footnote could not be written would be a worse lie than the missing line.

`onevcs publish` takes the body two ways: `--body` and `--body-file`.
The first is the text as typed and the second is the path of a file holding it,
which is the form a body of real prose arrives in — a shell argument is not where
multi-line Markdown survives. Passing both is refused by name before anything is
published, because two bodies is a caller that meant one of them and a publication
that guessed would open a change request nobody wrote. Neither branch-keyed verb
takes one: `recover` and `publish-branch` are reached by an operator naming a
branch, not by a caller that drafted a body.

**The repository's own merge path is the only verifier, and the rules file no longer
names one.** The approved contract gives every rule and the `default:` a `gate:` —
`checks`, `pre-push`, or a `command:` this crate runs itself. That tier is removed,
runner and vocabulary together.

It was never a second opinion. For a local-first repository a `command:` gate is
literally the same work the `pre-push` hook then does at the publishing push, run
twice against the same tree. For a remote-first one it front-runs the host's required
checks and discards the answer, so a red gate refuses a change CI would have refused
five minutes later and a green one proves nothing CI will not re-prove. Where the
tier is *judged* rather than deterministic the duplication is worse than wasted: one
repository spent three hours on a verdict that never converged, because each roll of
the judge named a different rule and neither roll was the one the merge path would
make. A verifier that runs beside the real one and throws its answer away is worse
than no verifier.

So verification is the repository's, and this crate hands it what it needs rather
than repeating it: the host's required checks for a remote-first identity, the
`pre-push` hook for a local-first one. `onevcs repos --audit-gates` already reports
which of the two covers each identity, and that question matters more now, not less.

**The schema is versioned rather than broken.** A rules file is an operator's
document on their own host, so refusing every `onevcs` command the moment this build
landed — before anything could re-apply their rules — would be a worse failure than
reading a key this build has nothing to do with.

- At **version 1 and 2** a `gate:` on a rule or on `default:` is **accepted and
  ignored**, with one line on stderr naming the file it was read out of. Nothing else
  about those versions changes, and the approved fixture below is still a file this
  build reads.
- At **version 3** there is no such key: `Policy` is `{publication, approvals}`
  exactly, and a `gate:` anywhere is refused as the unknown field it is.

```yaml
version: 3
trailer_prefix: Onevcs-
rules:
  - match: {host: github.com, owner: acme-corp, name: "*"}
    publication: change-open          # local-direct | change-open | change-auto | change-direct
    approvals: required               # required | none
  - match: {path: "~/projects/*"}
    publication: local-direct
default: {publication: change-open, approvals: required}
```

Everything downstream of the field goes with it. `onevcs rules check REPO` prints
`publication:` and `approvals:` with their `(from …)` sources and no `gate:` line;
`onevcs repos` keeps its third column, which is the registry's own detected identity
bar and was never this field. Two of the approved text's event kinds are retired,
because nothing emits them any more:

Event kinds retired: `gate-started`, `gate-verdict`.

What a publishing push wrote is still evidence, on the `push` event, unconditionally
— which is where a `pre-push` verdict always actually arrived.

`FailureKind` and `Error` are **unchanged**, `Gate` included. The exit code the
contract fixes for a verification failure is still `1`, and the vocabulary is fixed
across the three libraries that route on it — so a variant is not renamed because the
tier behind it went away. `FailureKind::Gate` now means the repository's own
`commit-msg` hook turning down the subject a publication would land under, and a host
that took a merge and then reported it unperformed.

`status --json` bumps to **version 3**. `identity.gate` was the rules file's `gate:`
and there is no such key; the top-level `gate` was the last verdict a gate this crate
ran had recorded, and this crate runs none. The second is re-expressed as
`merge_path` — the same question, asked of the verifier that actually rules — read
off the `push` event:

```json
{"merge_path": {"verdict": "pass", "log": "<the preserved log>", "recorded_by": "<session>"}}
```

`verdict` is `pass`, `fail`, or `unrecorded` for a push that recorded no answer.
`identity.gate` gets no replacement: the identity's own detected bar is a different
field on a different document, and merge-path coverage is what `onevcs repos
--audit-gates` reports.

The local merge train (`onevcs integrate`) verified each candidate with the same
`command:` gate, and now verifies with the repository's `pre-push` hook at the push
that publishes the advanced base — the same verifier every other landing of a
local-first identity answers to. Only trains whose rules named a command ever ran
that per-candidate tier: `gate: {kind: pre-push}`, which is what the approved fixture
gives a local-first repository, ran nothing there. An identity whose merge path runs
nothing at all is warned about in the words `onevcs register` already uses, not
refused — a train that refused where a publication does not would send an operator to
raw `git merge`, which is verified by less.

`onevcs recover` still refuses to attest a branch that nothing verified. The guard
asked the rules file whether the resolved policy named `pre-push`; it asks the merge
path itself now, through the same coverage question `onevcs repos --audit-gates`
answers, so the guard, the registration warning, and the audit cannot disagree about
which identities are covered.

**A push that landed and a merge path that could not be read is its own failure.**
Everything a publication does after its publishing push is a read of the *host*, and
until now a read that failed was reported with a kind that says the publication did
not land: a host that would not answer surfaced as `Invalid`, and the work was on the
remote the whole time. Twice in one session a publication settled as failed while its
branch was on the remote and CI was running on it, and the two obvious reactions to
that status — re-running finished work, or reading a chain as still blocked — are
both wrong and both expensive.

So the vocabulary widens by one kind, which is the only thing a router that branches
on the kind can see:

```rust
pub enum FailureKind { Gate, Invalid, SyncConflict, NotImplemented,
                       ChecksFailed, ChecksUnsettled, PushRejected,
                       PushedUnverified }  // 1 | 2 | 3 | 70 | 1 | 1 | 1 | 1
```

`PushedUnverified` is a publishing push that reached the remote followed by a merge
path this build could not read, and its `reason` names both facts: where the push
landed, and what stopped the read. It keeps **exit code 1**, the code the contract
already fixes for a verification failure, so a process that only reads `$?` sees
nothing change. `Error` gains the matching variant; it is `#[non_exhaustive]`, which
is what makes that additive.

It narrows rather than absorbs. A required check that concluded red is still
`ChecksFailed`, the watch's own bound elapsing is still `ChecksUnsettled`, and a push
the merge path refused is still `PushRejected` — a refused push never reaches the
host reads at all. `NotImplemented` passes through it too: `70` is the code this
repository fixes for a seam with no body, and a seam nobody wrote is not a merge path
that could not be read. And so does `GateFailed`, which the amendment above fixes for
"a host that took a merge and then reported it unperformed": that case shares this
defect's shape — the push landed, and the operator is not told so — but the meaning
of a kind is the contract's, so it is reported here rather than re-pointed.

The one host answer that changes with it is the empty check list. `gh pr checks
--required` says `no required checks reported on the '<branch>' branch` for a
repository that declares none, and `no checks reported on the '<branch>' branch` for a
head nothing has reported on yet — the shape every head wears for the first seconds
after a push. The first is an answer and stays one, so a repository with no branch
protection is readable and publishable exactly as before. The second is a race and is
refused under its own wording, because reading it as the first would wave a merge
through on a head no verification has begun on.

**`onevcs` knows about the releases that follow a landed change, so an upgrade can
be sequenced behind the release that carries it.** The approved contract ends at the
merge: a change reaches its base and this crate has nothing more to say. So a plan
spanning several repositories had no way to express "this node needs the *released*
thing" as distinct from "this node needs the *work*", and an operator did that
sequencing by hand — holding a node back until they had watched a release go out, or
correcting a worker mid-run when it pinned against something since published.

Three things become knowable per repository: which release targets it has, whether
each is released automatically or needs a human step, and whether a given landed
change has been released yet.

**The style decides the shape of the configuration rather than labelling it.** An
*automated* target carries a probe and is answered by running it. A *human-step*
target carries no probe at all — there is nothing to ask, because the release
happens when a person does something — and is answered by an explicit record a
person writes afterwards. Those are different waits, and `style` is the tag over the
two shapes rather than a field beside them: a `human-step` target naming a `probe:`,
and an `automated` target naming an `action:`, each fail to load, naming the target.
"A human-step target has a probe" is not a state this crate can hold.

The targets live in a new tracked YAML document beside the rules file, reached the
same way: `Registry` gains an optional `releases` key next to `rules`, **and the
registry version does not move for it.** That is deliberate, and it is the one
place this amendment departs from what was asked for.

The registry is *shared host state*: every `onevcs` on a machine reads the one
document, and `store::load` rewrites it in place the moment it migrates. A version
this build writes and an already-released build cannot read therefore does not
degrade that build — it stops it, for every verb, on a host whose operator opted
into nothing, the first time any newer `onevcs` reads the file. So the key is
additive instead. Absent, it is omitted, and a host with no release targets has a
document byte for byte the one an older build already reads and this build does not
touch. Present, an older build refuses it by name through `deny_unknown_fields` —
which is the fail-closed answer a version bump was for, without the blast radius.
A host with no release-targets document at all behaves exactly as it did before
there was one: every repository has no release targets and adopts fast.

```yaml
version: 1
default:
  adoption: fast                      # fast | published — the global rung
repositories:
  - match: {host: github.com, owner: nickderobertis, name: onevcs}
    adoption: published               # the per-repository rung; unset falls to default
    default_target: crate             # what a consumer naming no target gets
    targets:
      - name: crate
        style: automated
        probe:
          shell: 'npm view onevcs-cli version'
          timeout_seconds: 60
      - name: wheel
        style: automated
        probe:
          script: scripts/probe-released-wheel.sh
          args: []
          timeout_seconds: 60
      - name: container
        style: human-step
        action: "Push the image to the internal registry and record the tag."
```

`match` is the rules file's own `rules::RuleMatch`, with the same first-match-wins
semantics, reused rather than re-declared: two match vocabularies over the same
identities would drift.

**The two probe forms, neither privileged.** Exactly one of `script:` and `shell:`
per automated target; both or neither is refused when the document loads, naming the
target. `script:` is a path relative to the repository root, checked into the
repository being released, and runs as a **direct subprocess, never through a
shell**, from the identity's registered publication checkout at that identity's base
branch — never a run clone, a session worktree, or a branch under review, because a
probe reading a script off the branch a dispatch is authoring is a probe that
dispatch can rewrite. An identity with no registered checkout, or one not on its
base, cannot run that form: the probe answers "not answered" carrying that reason
rather than failing anything. `shell:` is a one-liner configured on this host, run
through `sh -c` in a temporary working directory. Both run under a **mandatory
bounded timeout defaulting to 60 seconds** — `timeout_seconds` is optional and never
unbounded — and with an explicitly constructed environment rather than the caller's
inherited one.

**What a probe prints, and how its answer is read.** One line on stdout and an exit
status: exit 0 with a non-empty first line is that version; exit 0 with empty output
is "no release yet"; anything else — a non-zero exit, a timeout, a spawn failure,
output that is not a single usable line — is **not answered**. A probe's output is
**untrusted data**: it is parsed, and it reaches no shell, no message template, and
nothing later rendered into one.

**"Not answered" and "not released" are different answers and stay different** all
the way up — through the library answer, the command rendering, and the event
payload. A consumer holds indefinitely on the first and never reads it as evidence
that a release has not happened; collapsing the two is the single most damaging
thing this could get wrong.

**The release baseline.** When a publication lands, this crate probes each of that
identity's **automated** targets and records what it found against the landing
commit, in `$ONEVCS_HOME/releases/<identity>.json`. All three answers are persisted
distinctly, as a tagged object rather than a bare version string — a bare string
cannot express the other two, and conflating any pair of them is how a change gets
reported as released when it is not:

```jsonc
"baselines": {
  "crate":  { "<landing_commit>": {"state": "at", "version": "0.12.2"} },
  "wheel":  { "<landing_commit>": {"state": "no-release"} },
  "npm":    { "<landing_commit>": {"state": "unestablished",
                                   "reason": "probe timed out after 60s",
                                   "attempted_at": "2026-08-23T17:04:11.412Z"} }
}
```

Comparison follows the baseline. `At { version }` is released once the probe answers
a version **strictly greater under semantic-version ordering**; until then the answer
is not released, carrying the landing version and the current one, and a version on
either side that is not a semantic version answers "not answered" naming which side
— a string comparison would report a yank or a re-tag as a release. `NoRelease` means
the target had nothing at all when the change landed, so **the first version the
probe ever answers is the release that carries it**, whatever its number: there is
nothing to be strictly greater than, and requiring a comparison would hold such a
change unreleased for ever.

**An unestablished record is never treated as a baseline, and cannot become a
trustworthy one by waiting.** A probe that did not answer at landing left this crate
not knowing what was released then, and a probe that answers a *version* later cannot
repair that — the release carrying this very change may already be included in it. So
`release status` for such a landing answers **"not answered"**, naming that no
baseline was captured, the reason the probe gave then, and that a comparison would be
unsound; it never degrades into a version comparison and is never reported as "not
released". Establishment is re-attempted on later asks, and exactly one later answer
repairs it: a probe answering **`NoRelease`** establishes `Baseline::NoRelease`
soundly and retroactively, because nothing being released now proves nothing was
released at landing. There is no other automatic recovery and none should be added;
the way out is a person's — fix the probe and land again, or adopt fast. A landing
this crate never probed at all is the same state and is answered the same way.

Nothing is probed at landing for a human-step target, because there is nothing to
probe. What the landing starts for one of those is a wait, measured from the landing
commit's own committer date.

**A human-step release is learned about the only way it can be: somebody says so.**
One acknowledgement per `(identity, target, landing_commit)`, in the *same* document
the baselines live in, so one read answers both styles:

```jsonc
{
  "version": 1,
  "identity": "github.com/nickderobertis/onevcs",
  "baselines":        { "crate":     { "<landing_commit>": {"state": "at", "version": "0.12.2"} } },
  "acknowledgements": { "container": { "<landing_commit>": {
      "version": "2026.8.23",
      "recorded_at": "2026-08-23T17:04:11.412Z",
      "actor": "nick",
      "superseded": [ {"version": "2026.8.22", "recorded_at": "…", "actor": "nick"} ]
  } } },
  "observed":         { "container": { "<landing_commit>": "2026.8.23" } }
}
```

Written with the atomic whole-document replacement and the process-shared lock this
crate already uses for the registry, so a concurrent reader never sees half of it.
`observed` is what makes `release-observed` fire the *first* time a landing is
released and not on every later ask. The actor is `ONEVCS_ACTOR`, otherwise `USER`
or `LOGNAME`, otherwise `unknown`: this operation reaches no `RemoteHost`, by the
rule that keeps `session_holders` off one, so there is nobody to ask. It is
persisted, carried on an event, and printed, so it is checked where it arrives —
one line, not blank, at most 128 characters. `ONEVCS_ACTOR` is this crate's own
knob, so an unusable value there is refused by name as every other `ONEVCS_` knob's
is; `USER` and `LOGNAME` are the environment's, so an unusable one is simply not an
actor and the next source is asked.

It refuses, each naming what to do instead: a target whose style is `automated` —
its version comes from its probe, and a hand-written second answer is exactly the
disagreement this design avoids — a reference that has not landed, a version that is
not a semantic version, and a target the repository does not declare. Recording the
same version again succeeds and changes nothing, re-reporting the existing record
with its **original** timestamp and actor, because a retried command and a second
operator doing the same thing both have to be safe. Recording a *different* version
is **refused**, naming the version already recorded and the invocation that would
replace it — a consumer may already have read the first answer and started work on
it. `--supersede` is that explicit replacement, and it keeps the previous version in
the record's own `superseded` history.

```rust
pub struct ReleasesFile { pub version: u32, pub repositories: Vec<ReleaseRule>,
                          pub default: ReleaseDefault }
pub struct ReleaseDefault { pub adoption: Adoption }
pub struct ReleaseRule { pub r#match: rules::RuleMatch, pub adoption: Option<Adoption>,
                         pub default_target: Option<TargetName>,
                         pub targets: Vec<ReleaseTarget> }
pub struct ReleaseTarget { pub name: TargetName, pub release: ReleaseMethod }
pub struct TargetName(String);                        // TryFrom<String>, validated
pub enum Adoption { Fast, Published }                 // fast | published

/// How this target is released, and therefore how a release of it is learned about.
/// The probe lives on the automated variant, so a human-step target has none to run.
#[serde(tag = "style", rename_all = "kebab-case")]
pub enum ReleaseMethod {
    Automated { probe: Probe },
    HumanStep { action: String },       // what a person has to do, rendered in the wait
}

impl ReleaseTarget {
    pub fn style(&self) -> ReleaseStyle;    // the label, for reporting
    pub fn probe(&self) -> Option<&Probe>;  // always None for a human-step target
    pub fn action(&self) -> Option<&str>;   // …and always Some for one
}
pub enum ReleaseStyle { Automated, HumanStep }        // automated | human-step
impl ReleaseStyle { pub fn as_str(&self) -> &'static str; }
impl Adoption { pub fn as_str(&self) -> &'static str; }

pub enum Probe {
    Script { script: PathBuf, args: Vec<String>, timeout_seconds: u64 },
    Shell  { shell: String, timeout_seconds: u64 },
}
impl Probe { pub fn form(&self) -> &'static str; }    // `script` | `shell`, as the event spells it
pub enum ReleaseAnswer {
    Released { version: String },
    NoRelease,
    NotAnswered { reason: String },
}
pub enum Baseline { At { version: String }, NoRelease }
pub enum BaselineRecord { Established(Baseline),
                          Unestablished { reason: String, attempted_at: String } }
pub enum ReleaseStatus {
    Released { target: TargetName, style: ReleaseStyle, version: String },
    /// Automated only: a probe answered, and the baseline has not been passed.
    NotReleased { at_landing: Baseline, now: String },
    /// Human step only: it landed, and nobody has acknowledged a release yet.
    /// Neither `NotReleased` (no probe answered) nor `NotAnswered` (no probe failed).
    AwaitingHumanStep { target: TargetName, action: String, since: String },
    NotAnswered { reason: String },
    NotLanded,
}
pub struct Acknowledgement { pub identity: String, pub target: TargetName,
                             pub landing_commit: String, pub version: String,
                             pub recorded_at: String, pub actor: String,
                             pub superseded: Vec<SupersededRelease> }
pub struct SupersededRelease { pub version: String, pub recorded_at: String,
                               pub actor: String }
pub struct RepositoryReleases { pub identity: String, pub adoption: Adoption,
                                pub default_target: Option<TargetName>,
                                pub targets: Vec<ReleaseTarget> }
impl RepositoryReleases {
    /// The target a name selects, or the reason no target answers to it: a caller
    /// naming none gets `default_target`, and a repository declaring none says what
    /// it does declare rather than guessing which artifact is depended on.
    pub fn select(&self, named: Option<&TargetName>) -> Result<&ReleaseTarget>;
}

pub fn release_targets(repo: &str) -> Result<RepositoryReleases>;
pub fn release_latest(repo: &str, target: Option<&TargetName>) -> Result<ReleaseAnswer>;
pub fn release_status(reference: &str, target: Option<&TargetName>) -> Result<ReleaseStatus>;
pub fn acknowledge_release(reference: &str, target: &TargetName, version: &str,
                           supersede: bool) -> Result<Acknowledgement>;
pub fn adoption_for(repo: &str) -> Result<Adoption>;
```

`ReleaseStatus::NotReleased` carries the *baseline* rather than a bare string, so
"no release at landing" is a state it can express, and `now` is empty where there is
no release right now at all — which is what a target answers before its first release
and after a yank. `release_latest` on a human-step target executes nothing: it
answers from the newest acknowledgement across that target's landings, or `NoRelease`
where none has been recorded, and the probe-failure reasons cannot arise for it
because no probe ran. `adoption_for` answers the repository rung when a rule sets one
and the global rung otherwise; it never answers the node rung and never defaults to
`fast` itself, because those two rungs belong to the consumer and a crate that
answered all four would make the chain unreadable from either side. None of the five
takes `Providers`, for the reason `session_holders` does not: what a repository
releases is this host's own configuration and its own record, and there is nothing
there for an implementation of either interface to answer.

Four verbs, each also taking `--json`: `onevcs release targets REPO`, `onevcs
release latest REPO [--target NAME]`, `onevcs release status REF [--target NAME]`,
and `onevcs release acknowledge REF --target NAME --version VERSION
[--supersede]`. `REPO` is the identity key, registered alias, origin URL, or path
every other command takes, and `REF` is the four-spelling reference `onevcs status`
takes. The approved usage block below is committed verbatim, so — as with every
verb added since it was written — the block a parser is reconciled against is the
one in the inferred-surface record.

Event kinds added: `release-probed`, `release-acknowledged`, `release-observed`.

- `release-probed` — `{identity, target, form, outcome, version, elapsed_ms}`, where
  `form` is `script` or `shell` and `outcome` is `released`, `no-release`, or
  `not-answered`; `version` is present only where a version was answered. **Emitted
  for automated targets only.** A human-step target never produces one, and that
  absence is the observable proof that no probe ran for it. A probe run while a
  publication is capturing its baselines is recorded on that session's own stream;
  every other one is recorded on the identity's release stream.
- `release-acknowledged` — `{identity, target, version, landing_commit, actor,
  superseded}`, emitted when the acknowledge operation records a release.
  `superseded` is the version it replaced, and is absent on a first record.
- `release-observed` — `{identity, target, style, version, landing_commit}`, emitted
  the first time a landing is released — its baseline passed for an automated target,
  its acknowledgement recorded for a human-step one. One kind for both, because a
  consumer renders it as "the release that carried this work" either way, and `style`
  is what says which kind of release that was. `landing_commit` is the only thing
  that correlates it: this event fires long after the dispatch that produced the work
  has ended, outside any session, so nothing downstream can stamp it with a node. It
  is therefore never absent and never abbreviated.

**A probe is not a gate.** Version 0.11.0 removed the `gate:` concept deliberately —
a verifier beside the real one that threw its answer away — and nothing here
reintroduces one. A probe answers what version is out there; it never rules on a
change, never refuses a publication, and never sits between a branch and its merge
path.

Nothing about publication, recovery, integration, or the rules file changes.

---

### Shared event envelope (duplicate these types in this crate; there is deliberately no shared util crate)

Every process in the stack emits NDJSON, one envelope shape:

```json
{"v": 1, "ts": "<RFC3339, millisecond, UTC>", "stream": "<unique id per producing process>",
 "seq": 42, "source": "agentgraph|vcs|pipeline", "kind": "<event kind>",
 "labels": {"run_id": "R", "round": 2, "node": "service", "step": "implement",
            "member": "worker", "persona": "engineer"},
 "payload": {}, "artifacts": [{"id": "a-91", "kind": "log", "bytes": 21400}]}
```

- `seq` is a u64, monotonic per `stream`. Merge order across streams is `(ts, stream, seq)`; a consumer detects loss via per-stream seq gaps. No cross-stream ordering promises beyond timestamps.
- `labels`: the reserved keys shown plus free-form extras; producers stamp what they know, enrichers never rewrite.
- Bounded payloads: payload text fields truncate at 4096 bytes with `"truncated": true`. Large evidence (gate logs, check logs, transcripts) is an artifact: stored by the producing library, referenced by id, fetched via that library's CLI.
- Redaction of credential-shaped values happens before an event or artifact leaves the producing library.
- Text output mode is a deterministic rendering of the same events, never separate content.
- A cross-repo contract test asserts this crate's envelope serialization against the spec fixtures committed in docs/contract.md.

### onevcs contract

VCS + remote-host abstraction with a trait seam, plus a rules system. Host-neutral vocabulary: the review unit is a **change request** (GitHub maps it to a pull request; GitLab later to a merge request).

```rust
pub trait Vcs {                        // impl now: Git
    fn resolve_identity(&self, origin_or_path: &str) -> Result<Identity>;
    fn open_session(&self, req: SessionRequest) -> Result<Session>;
    fn adopt_session(&self, token: SessionToken) -> Result<Session>;
    fn preserve(&self, s: &Session, provenance: Provenance) -> Result<PreservedBranch>;
    fn recoverable(&self, scope: Scope) -> Result<Vec<Recoverable>>;
}
pub trait RemoteHost {                 // impl now: GitHub (via gh)
    fn authenticated_user(&self) -> Result<String>;
    fn open_change(&self, req: ChangeSpec) -> Result<ChangeRequest>;
    fn find_changes(&self, head: &str, base: &str) -> Result<Vec<ChangeRequest>>;
    fn change_checks(&self, cr: &ChangeRequest) -> Result<Vec<Check>>;  // name, status, conclusion, required: bool
    fn check_log(&self, cr: &ChangeRequest, check: &Check) -> Result<ArtifactId>;
    fn merge(&self, cr: &ChangeRequest, policy: MergePolicy) -> Result<MergeOutcome>;
}
pub struct ChangeRequest { pub id: ChangeId, pub url: Url, pub head_sha: Sha, pub base: String }
pub struct Session {   // per-run shared clone + worktree, occupancy-leased
    pub token: SessionToken, pub worktree: PathBuf, pub branch: String, pub base: String
}
```

Registry: versioned JSON (v5 = ai-orchestrator's v4 identities/checkouts + rules reference), atomic replace under process-shared locks, lazy migration from v2-v4.

Rules (YAML, first match wins):

```yaml
version: 1
rules:
  - match: {host: github.com, owner: acme-corp, name: "*"}
    publication: change-open          # local-direct | change-open | change-auto | change-direct
    approvals: required               # required | none
    gate: {kind: checks}              # checks | pre-push | command: [...]
  - match: {path: "~/projects/*"}
    publication: local-direct
    gate: {kind: pre-push}
default: {publication: change-open, approvals: required, gate: {kind: checks}}
```

Legacy mapping: team -> change-open + approvals required; single-owner local -> local-direct; single-owner remote -> change-auto. A per-run explicit policy may narrow (change-auto -> change-open) but never widen past `approvals: required`. Stack metadata fields are host-neutral: `change_url`, `change_base`.

CLI:

```
onevcs register PATH [--origin URL] | repos [--audit-gates] | resolve REPO
onevcs session open REPO [--branch B] [--base B] [--execution-checkout ALIAS]
onevcs session adopt TOKEN | session close TOKEN
onevcs session holders REPO [--json]
onevcs publish TOKEN [--policy P] [--title T]
onevcs recover BRANCH --repo PATH | recoverable [--json]
onevcs integrate BRANCH... [--push] | sync [BRANCH]
onevcs events TOKEN [--follow] | artifact cat ID
onevcs rules check REPO
```

`publish` exit: 0 merged or change-open per policy; 1 gate/checks failed (stream names the check, artifacts carry logs); 2 invalid; 3 sync-conflict after bounded resolve-and-requeue.

Event kinds: `session-opened`, `fetch`, `lock-wait` (identity, elapsed, queue_position), `lock-acquired`, `gate-started`, `gate-verdict` (pass/fail, log artifact), `commit-preserved` (provenance kind), `push`, `change-opened` (url, host kind), `change-check` (name, required, status transition, conclusion, log artifact on completion), `change-merged`, `merge-queued`, `merge-completed`, `recovery-attested`, `sync-conflict`, `session-closed`.

Ported invariants: publication checkout never worked in / ff-only; per-run `--shared --no-checkout` clones with gc protection on lenders; flock queueing with fetches outside exclusive sections; every git command bounded by timeout; newest-3 dead-run retention; dirty adoption -> incomplete-step commit; one commit per base advance with `Recovered-Incomplete` trailers; gate environment passthrough (`comparison_env`) so a publishing push replays the worker's judged verdict.
