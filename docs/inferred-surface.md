# What the contract left to inference

The approved contract names every public item this crate exposes, but it does not
spell every field of every type it names. Where a shape had to be
chosen to make the contract compile, it is recorded here — so a reviewer can see
exactly which lines are approved and which are an inference waiting to be
confirmed, and so the next task extends this record instead of inventing again.

Nothing here is a licence to add a public item the contract does not name. When
one of these turns out to be wrong, it is corrected as a contract amendment, not
quietly in passing.

## Types the contract names but does not lay out

| Type | Inferred shape | Why |
| --- | --- | --- |
| `Identity` | `origin`, `workflow`, `repo_type`, `gate` | The contract says the registry is "v5 = ai-orchestrator's v4 identities/checkouts + rules reference", so the identity record is v4's, field for field. The identity *key* is not a field: it is the normalized origin, which is the map key in the document. |
| `Checkout` | `path`, `identity` | v4's checkout record. |
| `Registry` | `version`, `identities`, `checkouts`, `rules` | v4's document plus the rules reference the contract adds. `rules` is optional: absent means the built-in default policy. |
| `SessionRequest` | `repo`, `branch`, `base`, `execution_checkout` | Exactly the operands and options `onevcs session open` takes. What each of `branch` and `base` *means* is inferred too, and is open question 12 below. |
| `SessionToken` | newtype over `String` | Opaque by design; the CLI takes and prints it as text. |
| `Provenance` | `complete` / `incomplete-step` | The contract's ported invariant, "dirty adoption -> incomplete-step commit", gives the two cases, and `commit-preserved` carries "provenance kind". |
| `PreservedBranch` | `branch`, `base`, `provenance`, `change_url`, `change_base` | The last two are named explicitly as the host-neutral stack metadata; the first three are what `preserve` must return to be usable. |
| `Scope` | `all` / `repo(String)` | `recoverable` is documented both across every registered identity and for one repository (`onevcs recover BRANCH --repo PATH`). |
| `Recoverable` | `identity`, `branch`, `checkout`, `landed`, `stopped_because`, `recover_command`, `held_by`, `net_negative` | What a "recoverable" view has to answer: where the work is, whether the work reached its base, why its workstream stopped, and the exact command that lands it. `landed`, `held_by`, and `net_negative` are what make "the exact command" true of the branch as well as of the argv, and each is recorded below. |
| `ChangeSpec` | `head`, `base`, `title`, `body` | `open_change` must say what to open from, into what, and under what title — `--title` is a `publish` option. `body` is optional so the host's own template applies when nothing is supplied. |
| `MergeOutcome` | `merged(Sha)` / `queued` / `open` | The three ways `publish` exits 0, plus the `merge-queued` / `merge-completed` events. |
| `Check.status` / `Check.conclusion` | `String` / `Option<String>` | See the open question below. |
| `ArtifactRef.kind` | `String` | The contract shows only `log` and names no closed set. |
| `GitHub` | `{ repo }` | The contract names the implementation ("impl now: GitHub (via `gh`)") and lays out no shape for it. Every method it has is addressed to one repository, and the trait's methods do not carry one, so the implementation holds it. |
| `Git` | a unit struct | Nothing it does is per-instance: the registry, the workspaces, and the locks are all under the one state root, so two callers see the same host whether or not they share a value. |

## Types the contract implies but does not name

Three exist because a named item could not be written without them:

- **`Policy`** — the object under `default:` in the rules file, and the shape a
  rule resolves to. A `Rule`'s three policy fields are each optional (the second
  rule in the contract's own fixture omits `approvals`); a `Policy`'s are not.
- **`GateKind`** — `checks` and `pre-push`, the two values the contract's `gate:
  {kind: ...}` comment lists. The third form, `command: [...]`, is the other
  variant of `Gate` itself.
- **`Approvals`** — `required` / `none`, the values of the rules file's
  `approvals:` key.

`MergePolicy` is deliberately *not* duplicated: the rules file's `publication:`
key and `RemoteHost::merge`'s `policy` argument list the same four values, so they
are one type. `--policy` and the rules file are held to the same spelling by the
contract suite.

## Reaching the two interfaces: what the contract declares but does not route

The contract declares `Vcs` and `RemoteHost` "with a trait seam" and names `Git`
and `GitHub` as the implementations there are now. It says nothing about how a run
*reaches* either — and for a while nothing did: every call site named the concrete
type, so a second implementation could not be supplied to the interface it
satisfied. Three items exist to close that, and each is the smallest thing that
could:

- **`Hosting`** — `fn for_repo(&self, slug: &str) -> Result<Box<dyn RemoteHost>>`.
  A host is addressed at a repository, not at an installation: every `gh`
  invocation carries a slug, and `RemoteHost`'s own methods carry none. So what a
  caller supplies is the factory rather than one host, and `GitHub::new` stays the
  only way a `GitHub` is constructed.
- **`Providers<'a>`** — `{ vcs: &'a dyn Vcs, hosting: &'a dyn Hosting }`, plus
  `Providers::real()` for `Git` + GitHub. Borrowed rather than owned, because what
  a supplied implementation recorded is what a caller wants to read afterwards.
  The GitHub factory behind `real()` is deliberately *not* public: a caller mixing
  a real GitHub into a run whose repository side is not git is asking for a
  combination neither half was written for, and exporting it would be a public item
  with no use.
- **`run_with(&Cli, Providers)`** — `run` is this with `Providers::real()`, so the
  contract's `run` is unchanged in signature and in behaviour.

One consequence worth stating plainly, because it is what the seam does *not*
reach:

- **A non-GitHub hosted origin still answers `NotImplemented`.** The slug a change
  request is opened against is derived from a `github.com/...` identity key, and
  that derivation is upstream of the factory. So supplying a `Hosting` does not
  make a GitLab origin publishable; it makes GitHub's *behaviour* replaceable.
  Routing a second host vocabulary through the seam is the next question, not this
  one.

**A publication's repository side used to be git rather than `Vcs`, and no longer
is.** The five methods covered identities, sessions, preserved work, and recovery,
while the work `onevcs publish` does — fetch, merge, squash, push — sat beneath
them in a private module, reached from a private on-disk session record only `Git`
wrote. That is why a session a supplied implementation opened was refused by
`publish` and by `session close`. The widening that closes it is an approved
amendment, written into `docs/contract.md`: `Vcs` owns the session record, closing
a session, and publishing one, so a provider-opened session is a first-class
session everywhere. What is *inferred* here is only the shape of the four types
that widening needs, and each is the smallest thing that could answer the question
it exists for. The declarations themselves live in `docs/contract.md` and are held
to the code by `the_amendment_declares_the_types_the_widened_seam_gained` in
`tests/contract.rs`; the column below records only *why* each shape was chosen,
which is what this file is for. The one row that names a field list a consumer
reads before it builds anything — `PublishRequest` — is reconciled with the type
itself by `the_inferred_surface_row_lists_the_fields_publish_request_actually_has`
beside it, so the rationale cannot come to describe options the request does not
take.

<!-- llmlint: ignore[contracts_have_one_source_or_a_drift_gate] the table below is a
reviewer's record of which lines are approved and which are an inference, not a second
declaration of them: the authoritative one is the amendment in docs/contract.md, and the
suite reconciles that with the types. Gating a rationale column would hold the reasons to
the code rather than the shapes, and the pre-existing rows above have the same character
for the same reason. -->

| Type | Inferred shape | Why |
| --- | --- | --- |
| `SessionRecord` | `session`, `identity`, `lifecycle`, `provenance` | What every command that takes a token needed off the private record and could not derive from a `Session`: which repository it belongs to, whether it is still open, and whether its branch carries an incomplete-step marker. |
| `PublishRequest` | `policy`, `title`, `body` | Exactly the options `onevcs publish` takes beyond the token. `title` is a `Subject` rather than a `String`: a publication commits and merges before it composes a message, so the check has to be in the conversion that builds the request rather than where the message is composed. `body` is a plain `String` for the opposite reason — a host places no shape on prose, so there is nothing for a conversion to check and an unusable body does not exist. |
| `Publication` | `session`, `branch`, `policy`, `outcome` | What a caller journals about a publication: which session and branch, the policy it was actually taken under (after the rules file and any narrowing), and what happened. |
| `PublishOutcome` | `merged` / `change-open` / `queued` / `nothing-to-publish` / `failed` | The four endings the CLI printed as prose, plus the failure it printed to stderr and reported as an exit code. `Retention` is on the failure because the branch is the only record of the work, and whether it survived is the first thing a caller asks. |

**A host's checks used to be a bare `Vec<Check>`, and no longer are.** The
credential decides which of GitHub's check sources can be read at all, and one
credential class — the fine-grained personal access token — can read none of the
ones the rollup is built from, whatever it is scoped to. So *where the answer came
from* is part of the answer, and that widening is an approved amendment written into
`docs/contract.md` and held to the code by
`the_amendment_declares_what_a_hosts_checks_say_about_where_they_came_from` in
`tests/contract.rs`. What is inferred here is only the shape:

| Type | Inferred shape | Why |
| --- | --- | --- |
| `ChangeChecks` | `checks`, `sources` | The list the contract already named, plus the one thing a caller cannot recover from it: which sources it is the list *of*. A field on `Check` would not answer it — an empty answer has no check to carry the source, and "GitHub Actions reported nothing" and "nothing was readable" are the two things that must never look alike. |
| `CheckSource` | `status-checks` / `actions` / `branch-rules` | Not a vocabulary this crate invented: they are the three endpoints an answer can be assembled from, and the third is separate because which checks *block* is a different question from what the checks are, answered by a different endpoint with a different reach. |

Deliberately *not* added: a `Check.source`, a source on the `change-check` event, and
a public constant for `ONEVCS_CHECK_SOURCE`. The first is the field above; the second
would make an event stream differ by credential, which is what `honesty.rs` compares
two backends on; the third is an operator knob like the timeouts beside it, and none
of those is a public item either.

Every type reachable from a supplied implementation's state also gained
`Deserialize` beside its `Serialize` — `Session`, `SessionToken`, `Provenance`,
`PreservedBranch`, `Recoverable`, `Scope`, `SessionRequest`, `ChangeRequest`,
`ChangeId`, `Sha`, `Check`, `ChangeSpec`, and `MergeOutcome`. Reading a state back
is what makes a scenario something a test can write down, and `onepipeline` had
already recorded the two it could not read (`SessionToken`, `MergeOutcome`) as
mirrors waiting to be deleted.

**The holder view existed and could not be reached, and now is a library call.**
`onevcs session holders` had rendered it since the first release, from a private
type over a private record read by a private function — so the only consumer that
wanted it had to spawn the binary and parse what it printed, which for that consumer
is the thing it composes libraries to avoid. The widening is an approved amendment
in `docs/contract.md`, held to the code by
`the_amendment_declares_the_holder_enumeration_and_the_shape_it_answers` in
`tests/contract.rs`. The shapes were not chosen here — they are what the command
already printed — so what this record holds is why the surface is drawn where it is:

| Item | Shape | Why |
| --- | --- | --- |
| `SessionHolder` | `token`, `identity`, `branch`, `worktree`, `owner_pid`, `state`, `liveness` | Field for field what `--json` printed, so the two surfaces cannot diverge and a consumer can parse the command's output into the type. `token` is a `SessionToken` rather than the `String` the private type carried: it is the value the rest of the surface takes, and its `transparent` serialization leaves the JSON identical. |
| `Liveness` | `live` / `stale` | Reported rather than derived. A caller holding `owner_pid` cannot answer it — pids are reused, so a later process wearing a dead session's number reads as its owner — and the creation identity that settles it is on the private record. `as_str` is public with it so a caller renders the words the command does rather than inventing a second spelling. |
| `session_holders(repo)` | `&str` in, `Vec<SessionHolder>` out | The command's operand and its output. It takes no `Providers`: the holders are the records under this host's state root, so there is nothing here for a supplied implementation to answer. |

**A row that offers a command has to say when the command must not be run.** The
inferred shape above was a row and a verb, and for two releases that was read as "this
is ready to land" — so `recoverable` offered a paste-ready `publish-branch` for a branch
a live session was still committing to (four more commits followed the row), and for two
branches that would have stripped hundreds of lines. Neither is a wrong *row*: the work
is real and the verb is right. What was missing is the part of the answer that says
whether to run it now, so two optional fields carry it, and both are omitted when they
say nothing — a consumer that predates them reads the document it always read.

| Item | Shape | Why |
| --- | --- | --- |
| `Recoverable.held_by` | `Option<HeldBy>` | Present is the whole answer: the work has not stopped. Excluding the row instead would hide live work from the one report that lists work nobody has, which is the failure this report exists for. |
| `HeldBy` | `token`, `worktree`, `holding` | The token because acting on it means waiting for that session or closing it by name, and the worktree because that is where the work is being made. |
| `Holding` | `owner-running` / `run-root-occupied` | Reported rather than derived, and two values because the two are true at different times: a consumer holding a `Session` keeps the process that opened it (which is `Liveness::Live`), while the CLI takes an occupancy lease per command and outlives none of them, so what says a command is in there *now* is the lease. `because` is public with it so a caller renders the crate's own clause rather than inventing a second one. |
| `Recoverable.net_negative` | `Option<NetNegative>` | Marked, never excluded: a branch that deletes far more than it adds may be exactly right, and this report is not the thing that decides. Present only when it is net-negative, so absence is the other answer rather than a number a consumer has to compare. |
| `NetNegative` | a `LineChange` that removes more than it adds | The mark and its evidence are one value, so a row cannot carry a count saying the opposite of the field it is in, and the rule lives in one place rather than at the site that measures a branch and at every consumer reading one back. It serializes as the `LineChange` it holds, so `--json` carries the two counts either way, and a document naming a count that is not net-negative is refused where it is read. |
| `LineChange` | `added`, `removed` | Counted from the commit the branch forked from, because that is what the branch did; against a base that has moved on, every line the base gained would read as a line the branch removed and never touched. |

**Reading a session's events takes a filter, and the grammar was approved rather
than inferred.** The matcher fields, what conjoins, and which of `include` and
`exclude` wins are fixed across `onevcs`, `oneagentgraph`, and `onepipeline` and are
written into `docs/contract.md`, held to the code by
`the_amendment_declares_the_filter_a_stream_is_read_through` in `tests/contract.rs`.
What is inferred here is only how this crate spells it:

| Item | Shape | Why |
| --- | --- | --- |
| `EventMatcher` | one public type, every field `Option` | The grammar names matchers and their fields but no type. A named type rather than a map is what lets a consumer build a filter as a value — which is the whole point of the typed seam — and `Option` per field is "unset asks nothing", which is what the grammar says each field means. |
| `EventFilter::parse` | `&str` in, `Result<Self>` out | A consumer with a spec as text needs one entry point that refuses it the way the CLI does. It reads YAML, which is the language the grammar is written in and a superset of the JSON the CLI takes inline, so both forms are one parser rather than two that could disagree. |
| `Deserialize for EventFilter` | routed through the same validation | Hand-written rather than derived, so a filter embedded in a consumer's own configuration is refused by the same rules, with the same message naming the same matcher. A derived one would name the field and not which matcher carried it. |
| `--filter SPEC` | inline when it opens with `{`, a path otherwise | Decided by the text rather than by whether a file happens to exist, so what an invocation means does not change with the directory it runs in. The grammar's document is a mapping, so the two forms cannot collide. |

Deliberately *not* public: `EventKind::wire`, the kebab-case spelling a `kind` glob
is matched against — it is `Serialize`'s answer, reachable that way already, and
`the_wire_spelling_of_every_kind_is_the_one_a_filter_matches` holds the two
together. Nor is there an `EventFilter` on `Stream`, the writing half: filtering
what a producer *records* is a different decision from filtering what a consumer
reads, and the stream is the record of what happened.

Deliberately *not* public: `workspace::Record` and `workspace::all`, which are the
whole durable record — the run root, the per-session clone, the two checkouts, and a
schema version this build refuses to read at any other value. Exporting them would
commit this crate to a private on-disk layout as a public type. `SessionHolder` is
the projection of it that answers the question, and `Ref`, `Token`, `ProcessStart`,
and `RECORD_VERSION` stay behind it for the same reason.

## The command surface, where a branch state had no verb

The contract's usage block gives a branch three ways to reach its base and leaves
one state with none. `publish` takes a session token; `integrate` lands a branch on
a **local** base and refuses an identity whose `repo_type` is `team` or whose
`workflow` is `remote`; `recover` publishes *interrupted* work and refuses a branch
carrying no unattested marker. A complete, unpublished branch whose session is gone
is therefore refused by both — and since every hosted origin `register` sees derives
as `team`/`remote`, that is every finished branch of every hosted repository, whose
only remaining exit was raw `git push` plus `gh pr create`.

The verb that answers it is **not in the approved text**, and the approved text is
never edited, so it is recorded here as an inference awaiting confirmation:

```
onevcs publish-branch BRANCH --repo PATH [--title T] [--body TEXT] [--body-file PATH] [--policy P]
onevcs recover BRANCH --repo PATH [--title T] [--body TEXT] [--body-file PATH]
onevcs status REF [--json]
onevcs import BRANCH --repo PATH [--from SOURCE] [--as NAME]
```

`tests/contract.rs` reads this block beside the contract's own and holds the parser
to the two together — the same equality as before, over both documents, so a command
the parser has that neither writes down still fails the gate, and so does one either
writes down that the parser does not have. `tests/e2e/support.rs` reads both for the
same reason.

| Item | Inferred shape | Why |
| --- | --- | --- |
| `publish-branch BRANCH --repo PATH` | the operands `recover` already takes | It is the same question asked of a different provenance, so it is reached the same way: a branch by name, and a checkout that resolves the identity. Anything else would be a second way to say the same thing. |
| `--policy P` | the option `publish` takes, under the same rule | The policy comes from the identity's rules; a per-run one may narrow it and never widen it past requiring approvals, which is `MergePolicy::narrow` rather than a second rule. |
| `--title T` on both verbs | the `Subject` an explicit title has always been | `publish` has taken one since the contract was written, and the refusal it answers — no commit subject fits — is reachable from a preserved branch too, where the alternative is rewriting a commit on work that was interrupted. |
| `--body TEXT` and `--body-file PATH` on both verbs | the pair `publish` takes, refused together by the same boundary | A body is drafted by whatever knew what the change was for, and which verb happened to land the branch is no reason for the change request to describe itself differently — an operator resuming a preserved branch reads the same empty pull request either way. Both spellings are representable so `app::explicit_body` can refuse the pair by name, and its refusal names the verb and operands the caller typed rather than `publish`'s. Nothing is composed when neither is given: the change request opens with no body, as it does today. |
| the answer | the same `PublishOutcome` and exit codes as `recover` | It is a second caller of the branch-keyed publication path, not a second publication. `recover` and `publish-branch` share one implementation of locate, clone, worktree, base-merge, gate, and publish; provenance is the whole of what separates them. |

No public library item is added: `publish::run` was already branch-keyed, and both
verbs are private modules behind it. What the CLI gains is the verb and four options.

**The body options depart from a sentence in the approved text**, which says that
neither branch-keyed verb takes a body because both are "reached by an operator
naming a branch, not by a caller that drafted a body". That premise stopped being
true: a caller now drafts bodies out of band and hands one to whichever verb lands
the branch, and the branch-keyed verbs are how most branches on this host land —
so the sentence's own reasoning, that the option belongs where there is something
to pass, is what puts it on all three verbs. The approved text is committed
verbatim and is not edited, so the departure is recorded here and in the open
question below; `PublishRequest` and the publication path are untouched, because
both already carry a body.

## Two more the contract does not name: reading, and ref plumbing

The same gap one step earlier. Every operation is supposed to go through `onevcs`
rather than through raw `git` and `gh`, and two questions had no verb at all — so an
agent that needed either answered it outside this boundary, which is the thing the
boundary exists to prevent.

- **Nothing answered "what became of this work".** `recoverable` answers what is
  *unpublished*. Nothing answered what was *proposed*, whether it *landed*, or how
  its checks went, so a dispatched worker ran `gh pr list` and a planner ran
  `gh pr checks` and `gh run view --log-failed`. Worse, the missing landed/unlanded
  answer produced a wrong conclusion: a change that had already squash-merged to
  `main` was reported as unpublished, because the only evidence consulted was the
  absence of an open pull request. `recoverable` was not blind to it — `vcs::collect`
  reads run clones and excludes a branch whose content the base already carries —
  but the *exclusion reason* was legible nowhere, so landed, still-held-by-a-live-
  session, and genuinely-preserved all read as silence.
- **Nothing made preserved work reachable.** Getting a branch out of a run clone and
  into a checkout a new run could clone took `git fetch <src> <branch>:<branch>`, and
  giving preserved work a name a fresh pin could safely take took
  `git branch preserved/<name> <branch>`. Both are ref plumbing over an identity's
  own checkouts, which belongs to the tool that owns the identity.

Neither verb adds a public library item: both are private modules, and what the CLI
gains is the two verbs and their options. Both names are **fixed** — a later node
wraps them in `just` recipes by name and gates on every subcommand this repository's
prose names existing in the pinned CLI's `--help`, so a rename breaks that node by
construction.

**`status --json` is a versioned object, and its bytes are checked in.** The report
leaves the process and is read by whoever consumes the command, which makes it the
same kind of thing as the registry document and the rules file: it declares its own
shape rather than leaving a consumer to infer one from which keys it can find.

The report's schema version is `2`, and it is deliberately not a migration boundary
— nothing in this build reads a report back, so the number is what a **consumer**
branches on and there is no older shape here to read. Version 2 is
`publication.landed` and the eighth `publication.state`, both recorded below.
Two rules follow, and they are the ones the goldens exist to enforce:

- **Every change to what the object carries bumps the version**, in the same change
  that re-makes the goldens. A field added, removed, renamed, or retyped without
  that is a consumer reading a shape it was told it would not get.
- **A field that holds nothing is omitted rather than written as `null`.** A
  consumer that has never heard of a field must not be handed one, and `null` and
  absent are different answers to "was there a session".
- **A version this build does not read is refused where the object is read**, by
  the conversion that decides it — so a document declaring one does not deserialize
  at all rather than becoming a value a reader has to remember to question. The
  number exists to be acted on, and reading a v2 document as a v1 one is reading
  fields that moved. A key nobody declared is refused for the reason the registry
  document refuses one: it is usually a typo for one that matters.

`crates/onevcs/tests/golden/status-report-v2.json` and
`status-report-v2-minimal.json` are those bytes — a report carrying every optional
field it can carry at once, and one carrying none of them — compared byte for byte
against the real CLI's own output by
`the_status_report_is_the_versioned_object_its_goldens_record` in
`tests/e2e/accounting.rs`, and read back *as reports* by `status::round_trip` — the
same two files, held from both sides, so the bytes a consumer meets and the shape it
parses them into cannot drift apart. That is also why a golden's stand-in for a
session token is a token (`s-000000000000`) rather than a placeholder shaped like
one: a golden nothing can parse is the opposite of what a golden is for. Two fields cannot share a golden with the rest and are
covered by name elsewhere: `next.command`, which no report carrying an open change
request has (there is nothing to advance), and `notes`, which reports a gap in what
could be *read* rather than anything about the work.

| Item | Inferred shape | Why |
| --- | --- | --- |
| `status REF` | one operand, four spellings, read in the documented order | A change request's URL, a session token, a branch name, and a commit are four names for one piece of work, and which one somebody has depends on where they are standing. Four options would make a caller say which they hold; one operand does not. First match wins, so a session token is a session token even where a branch of that name exists, and ambiguity is *within* a spelling — one branch name in two identities — which is refused naming every candidate. |
| the sections | identity, session, branch, publication, checks, gate, next | What an agent had to reach outside for, in one place. `next` is the surface the branch-keyed refusals already are: a report that diagnoses without naming the command that advances the work leaves an agent to invent one. |
| `landed` | decided from the base's own history, in four tiers, naming the one that decided it | Publication squashes, so a branch that landed is an ancestor of nothing afterwards — and the content comparison that used to answer this is an inference that stops being true the moment anything else lands on the base. The tiers are recorded below. It is the same question `vcs::collect` excludes a branch on, which is what keeps the report and `recoverable` from disagreeing about one branch. |
| the host section | degrades, never fails | `status` reaches the host for what a change request is doing now, and everything else it reports is answerable offline. A command that failed because a network call did would leave an operator with none of the answer. |
| `--json` | the same object, on stdout | The scope note `recoverable` carries does not apply: `status` is asked about one piece of work by name, so there is no unstated scope for a reader to mistake. |
| `import BRANCH --repo PATH` | the operands `recover` already takes | It is addressed at one branch of one identity, so it is reached the same way the two branch-keyed verbs are. Where it looks with no `--from` is `branch::locate` — the same search those verbs use, run clones included. |
| `--from SOURCE` | a repository path, or a remote ref | The two places a branch that is not already reachable can be. Decided by what the value *is* — a directory is a repository, anything else is a remote of the destination — rather than by trying each until one works, because a fetch that fell through to the wrong one would import somebody else's commit under this name. |
| `--as NAME` | an alternate local name | The `preserved/<name>` move, which exists because a session's branch pin is refused unless the base carries what the name means: work whose name is spent needs a second name before anything can take the first. |
| ref writes only | no checkout, no working tree | It fetches into a scratch ref, judges there, and points the destination's ref at it. A name the destination has checked out is refused rather than written: moving that ref leaves git holding a tree that describes a commit the branch no longer names. |
| the non-fast-forward refusal | names the commits that would be lost | A branch in a registered checkout is the durable record of whatever wrote it, and this verb is reached by somebody who wants a *second* copy reachable. `--as` is the way through, and it is only the right way through once an operator can see what the name they asked for already holds. |

## Whether work landed is decided from history, and the answer says what decided it

**A report that infers is a report that is sometimes wrong, and this one's wrong
answer is dangerous.** Whether a branch's work had reached its base was
`git diff --quiet <base> <branch>` over the whole tree, reported as a fact. It is an
inference and it is wrong as soon as anything else touches the base, related or not:
a branch that had landed read as work nobody published, and `recoverable` printed
`Resume: onevcs publish-branch <branch> --repo …` under it. That line is an
instruction, and following it on a landed branch re-opens a change request for work
`main` already carries. One report listed eighty-five preserved branches with three
just-merged ones at the top, each carrying it.

So landing is decided from history, in four tiers, most certain first, and the answer
names the tier that decided it. `landed.rs` is the one place they are asked, so
`status` and `recoverable` cannot come to disagree about one branch.

| Item | Inferred shape | Why |
| --- | --- | --- |
| tier 1 | a recorded landing: a landing commit recorded for the branch, which the base carries | Exact, permanent, and immune to whatever is edited afterwards. What writes one today is a merge this host *performed* — the `change-merged` and `merge-completed` events carry the commit — so a merge this host only waited for records nothing yet, and the field is defined so that the node which watches one land has somewhere to put it. The three tiers below are what answer until then. |
| tier 2 | the change request's number, in a commit the base's history carries, bounded by the fork point | The host writes it into the squash commit it lands, so it answers for anything merged through the host by anybody, with no write of ours and however far the base has moved since. Three spellings are matched — the number in a subject, a merge commit's sentence, and the URL itself — each with its own punctuation around it, so `#1` cannot answer for `#12`. |
| tier 3 | `<prefix>Landed-Commit:` on the base, naming a branch commit | The one landing that opens no change request is `local-direct`, and nothing else would ever say so. It names a commit rather than a branch name because a name is spent and re-cut. |
| tier 4 | the content comparison, over the paths the branch touched, and never a `yes` | Last, and labelled as the inference it is: what it can say is that the base does not carry what the branch changed (`no`), or that it does and nothing records why (`unknown`). Scoped to the paths the branch touched rather than the whole tree, so unrelated work landing beside it does not change the answer. |
| the guard on tiers 1–3 | the landing commit is asked whether it carries everything the branch changed since it forked; a branch holding anything it did not falls through | A landing lands what the branch carried *then*, and a session continuing a name that already means something commits onto the same branch — a row that read that as finished would hide unpublished work, the one direction this must never fail in. Asked of the *landing* commit rather than of the base as it stands now, which is what keeps the guard from being the inference the tiers replace: the base moves, and the commit that landed the work does not. |
| `Landed` | `yes` (carrying its evidence) / `no` / `unknown` | Three answers because the third is real: a branch that landed with no change request and not through this crate leaves nothing in history to read, and reporting that as `no` is what puts a resume instruction under work that is already on the base. The evidence travels *inside* the `yes`, so a landing with nothing behind it is unrepresentable. |
| `LandingEvidence` | `recorded-landing` / `change-request` / `trailer`, each with the commit | Naming the tier is half the answer: "it landed" is exactly the claim that used to be an inference, and a reader has to be able to tell a record from a comparison. |
| `publication.state` | the seven it had, plus `maybe-landed` | The eighth is the answer version 1 had no room for and reported as `landed`. `state` is derived from `landed`, so the two cannot disagree, and the human rendering's `landed:` line is the word it always was with the tier that decided it on the line below. |
| `Recoverable.landed` | the same value, on every row | The row is read to be pasted. A row whose work landed carries an **empty** `recover_command` rather than a command with a warning beside it, and no `Resume:` line in either rendering. |
| `Vcs::preserved(scope)` | `recoverable` plus the rows it withholds, defaulting to `recoverable` | `recoverable` answers "what is left to publish" and must never hand a caller a branch whose work is on the base. But an exclusion nobody can see is how preserved work goes missing, so those rows are reachable — through the seam, so a supplied implementation answers this command too, and with a default body so one that predates the question is unaffected. |
| `recoverable --all` | the flag that reaches it | Off by default: the report is read to decide what to publish, and both of the other two answers — it landed, and nothing here can tell — are answers that must not be acted on by pasting a command. Named `--all` rather than for landing, because `unknown` rows are withheld too. Every rendering, `--json` included, names the flag whether or not anything was withheld: what a report leaves out is exactly what nobody can see it left out. |

```
onevcs recoverable [--all] [--json]
```

## One more: the disk this tool fills, and the verb that empties it

The same gap again, one layer down. Every branch-keyed landing cuts a run root
under the state root — `workspaces/publications/<slug>-<unique>` for
`publish-branch` and `workspaces/recoveries/<slug>-<unique>` for `recover`, each
holding a clone, a worktree, and the gate's preserved logs — and **nothing has
ever removed one**. Measured on the host that motivated this: thirty-one
directories, forty-nine gigabytes, none of them ever reaped, because no verb this
tool has knew the directory existed. A full disk took that host down twice during
one three-day run and stopped the operator issuing any command at all.

<!-- llmlint: ignore-block[contracts_have_one_source_or_a_drift_gate] the half of this
surface the repository can reach is gated: `sweep::DEFAULT_MIN_AGE_HOURS` is the crate's
one source for the default, `cli.rs` takes the parser's from it, and the contract suite
holds it to the sentence below. The other half is a value in a repository this one does
not depend on and cannot build, so a check here would either vendor a copy of it — the
second source the rule exists to prevent — or reach the network from an offline gate.
Recording the shared surface is what lets the composing caller reconcile it, and is why
neither side may amend it alone. -->

```
onevcs sweep [--dry-run] [--min-age-hours HOURS]
```

The flag surface is **shared with `oneagentgraph sweep`**, spelling for spelling
and default for default, because one composing caller (`ai-orchestrator`'s
`just sweep-scratch`) forwards its own arguments to both unchanged. Neither side
may depart from it alone.

**Why the verb is here rather than in a general-purpose sweeper.** What makes a
publication workspace reclaimable is `onevcs` state: its gate has recorded a
verdict under it, and no live session holds its occupancy lease. Both are facts
this crate can read, and neither is one a caller should be asked to supply — a
caller-supplied liveness proof that is wrong deletes a publication worktree
somebody is still gating, and it is a seam no other crate could honestly test.
`lock::try_exclusive` over `workspace::occupancy_identity` answers the second, and
it is the same evidence `recoverable` uses to decide a branch is not being written
to; `branch::prepare` takes that lease for the whole of a landing so there is
something to read.

| Item | Inferred shape | Why |
| --- | --- | --- |
| `sweep` with no operand | the two families this crate cuts run roots under, and nothing else | It is asked about this tool's own disk, and a path operand would make it a general-purpose remover pointed at whatever a caller typed. `branch::Verb::ALL` is the list, so the verbs that make the directories and the verb that reaps them cannot come to disagree about where they are. |
| `--dry-run` | reports the same decisions and removes nothing | What a caller wants from a rehearsal is what the real run would decide, so the two runs differ in the removal alone. |
| `--min-age-hours HOURS` | a window, defaulting to 24 hours | Parsed into a `Duration` at the boundary, so nothing past the parser can be handed hours that are negative, infinite, or not a number. |
| the exit code | `0` whenever the sweep *ran* | A run root it could not prove dead, or could not remove, is a line in the report. A caller that got a non-zero code for a directory somebody else is inside could not tell that from a sweep that never happened. |
| the report | every family examined, every family it did not examine and why, what it reclaimed, and every retained directory with its reason | It is read to decide whether the disk is accounted for, and a directory that vanished from it reads as one nobody had to think about. |

**Retain rather than remove whenever the answer is not proven, and never kill.**
The state root is shared by several managers on one host. A run root whose owner
cannot be proven — one holding no run clone this crate would have cut — is
retained and reported; a run root a live session holds is retained, reported, and
never terminated. During the incident, 41 GB in a sibling orchestrator's root had
to be left untouched for exactly that reason, and a sweep that guessed would have
destroyed another manager's live work.

**One boundary is deliberately outside it.** `workspaces/<identity>/runs` is the
per-run lifecycle clone root, which `workspace::reclaim` keeps as a bounded
recovery history so a dead run's branch stays reachable. This verb reports it as a
family it does not reach into rather than reaping it. **`recoveries` is inside**,
and that was a decision rather than an oversight: it is the same directory shape
cut by the same function under the same two proofs, and leaving it out would mean
one of two families filling a disk that the other no longer does.

The age floor's default is `24` hours, and that number is the half of the shared
surface a caller never types — so it is the half that can drift without anybody
noticing. `the_sweep_age_floor_defaults_to_the_number_the_record_states` in
`tests/contract.rs` reads it back out of this sentence and holds clap's own default
to it, which means moving the number here or in the parser alone fails the gate.
What this repository cannot check is the *other* side of the shared surface: that
`oneagentgraph sweep` still spells the same option and answers to the same default.
That reconciliation belongs to the caller that composes the two, and is why neither
side may amend the surface alone.

The verb adds no public library item: `sweep` is a private module, and what the
CLI gains is the verb and its two options.
<!-- llmlint: ignore-end[contracts_have_one_source_or_a_drift_gate] the shared-surface
record ends here. -->

## One public item the contract does not name, and why it is not an inference

`provenance::SUBJECT_LIMIT` — the length a publication holds a commit subject to.
The contract names neither the constant nor the module, so this is the one place in
the crate where a public item exists that it does not list, and it is recorded here
rather than passed over.

It is not a shape somebody inferred: the **operator** raised the limit and directed
that `onepipeline` read it at `onevcs::provenance::SUBJECT_LIMIT` to validate a
plan's titles at load, before a publication would meet the same rule. A consumer
that restated the number instead would be a second copy of the rule that drifts the
first time it moves, which is the failure the rest of this file exists to prevent.

The exposure is the narrowest that gives the path: `provenance` is `pub mod`, and
**every other item in it is `pub(crate)`** — the trailer vocabulary, `Trailers`, and
the readers and writers around it are all still private. Widening any of them, or
naming the constant in `docs/contract.md`, is a contract amendment and not a
decision to take in passing.

## Open questions for the planner

These are reported rather than resolved. One that has since been resolved is kept
here, struck through and answered, so a reader meets the decision where the
question was:

1. **`Check.status` and `Check.conclusion` are untyped.** The contract fixes the
   field names and says `required: bool`, but enumerates no value set for the
   other two, and the vocabulary differs per host — which is the thing this crate
   exists to abstract. They are `String` and `Option<String>` today, so nothing is
   invented; a host-neutral enum (say `queued | in-progress | completed` and
   `success | failure | cancelled | skipped | neutral`) would be a contract
   amendment, not a local decision.
2. **`EventKind` is closed over this crate's kinds.** The envelope is shared and
   its `source` admits `agentgraph`, `vcs`, and `pipeline`, but the contract only
   enumerates the kinds `onevcs` produces. So this crate can *read* another
   source's envelope only if the kind happens to be one of its own. That is fine
   for a producer and for `onevcs events`; a consumer that merges all three
   streams (`onepipeline`) needs either its own superset or an `Other(String)`
   fallback variant here. Worth deciding once, across the three repositories,
   rather than three times.
3. **The bounded-payload constants are documented but not exposed.** The contract
   fixes truncation at 4096 bytes with `"truncated": true`, and the envelope at
   `v: 1`. Neither is a public item the contract names, so neither is exported. An
   implementer will want both; exporting them is a one-line amendment.
4. ~~**The provenance trailers are spelled by this crate and named by nothing.**~~
   **Resolved: the prefix is configurable, and the keys are no longer an
   inference.** They are an approved amendment, so they are written into the
   contract and nowhere else, and this entry deliberately repeats none of it. What
   belongs here is only why it left this list: a spelling nothing named is not a
   question a *reader* of this crate can answer, and the answer that generalizes is
   a hook rather than a second spelling this crate knows about.
5. **`Scope::Repo` is reached by where a command is run.** `onevcs recoverable`
   takes no repository operand, and the contract documents the view both across
   every identity and for one repository. Run inside a registered checkout it
   answers for that repository; run anywhere else, for all of them. An explicit
   operand would be a contract amendment.
6. **Holder enumeration does not go through the seam.** `session_holders` reads
   this host's session records, which is what `onevcs session holders` has always
   done and is why the two are one path — but it means a session a supplied `Vcs`
   opened into its own state is not in the list, the one place a session that
   implementation opened is not first-class. Closing it means a method on `Vcs`,
   which every implementor would have to write, so it is a contract amendment and a
   breaking release rather than a decision to take in passing.
7. **The filter grammar has no version, and one cannot be added from here.** Every
   other serialized shape in this crate is versioned, and each of those is written
   and read by this repository alone. A filter is written by whoever configures a
   run and read by `onevcs`, `oneagentgraph`, and `onepipeline`, so a `version` key
   one of them writes and the others refuse is the shared grammar ceasing to be
   shared. It is refused here like any other key the grammar does not have, which
   is what makes an unversioned document fail closed rather than half-read, and the
   amendment in `docs/contract.md` records the constraint. Versioning it is a
   proposal to raise with the contract owner across the three repositories — one
   spelling, one meaning for an absent version, one answer for what an older build
   does with a newer document — not a decision to take in passing.
8. **`onevcs publish-branch` is a verb the approved text does not name.** It
   closes a state the contract left with no verb at all, and the shape above is
   the one the downstream adoption node was written against — but the approved
   text is committed verbatim and an extension to it is the contract owner's to
   approve, so it is recorded in this document rather than written into
   `docs/contract.md` beside the amendments that were. Confirming it means one
   amendment naming the verb, its four options, and `recover`'s `--title`,
   `--body`, and `--body-file`; until then the surface is held to *this* record,
   which is what keeps the parser and a written-down surface reconciled rather
   than the verb being undocumented.
9. **A change request's URL is resolvable only through the event stream.** The URL
   is the host's name for a change, and nothing on the branch carries it — this
   crate reads a `<prefix>Change-Url:` trailer that something else may have written
   and writes none of its own. So `onevcs status URL` finds the work by the
   `change-opened` event the publication recorded, which means a change request
   opened by anything but this host's `onevcs` cannot be looked up by URL here.
   Closing it means either a `RemoteHost` method that reads a change request by its
   id — a required method every implementor would have to write — or a trailer this
   crate writes on the branch it publishes, which changes the bytes of every
   publication commit. Both are contract amendments rather than decisions to take
   in passing.
10. **`onevcs` stamps none of the envelope's reserved label keys.** It stamps
   `session` and `identity`, which are free-form extras, so a filter naming
   `run_id`, `node`, `step`, `member`, or `persona` admits nothing this crate
   produces — correctly, by the grammar's own rule, and the same answer a consumer
   would get from any producer that did not know the run around it. Whether a
   session should learn them (from its opener, or from the environment a run sets)
   is a question about what a session knows, not about filtering, and it would
   change the bytes of every stream — so it is reported here rather than taken in
   passing.
11. **The branch-keyed verbs now take a body, and the approved text says they do
   not.** The contract's sentence — "Neither branch-keyed verb takes one: `recover`
   and `publish-branch` are reached by an operator naming a branch, not by a caller
   that drafted a body" — was written when the only caller drafting one published
   through a session token. It no longer holds: bodies are drafted out of band, most
   branches on this host land through `publish-branch` after a run has ended, and a
   change request opened that way carried nothing a reviewer could read. So both
   verbs take `--body`/`--body-file`, and both hand it to the publication path
   unchanged. Nothing else moved — no body is composed when none is given, and the
   provenance trailers stay on the commit — so confirming this means one amendment
   striking that sentence, not a new shape to approve.
12. **A pinned branch that already exists is continued, and `base` is the branch a
   session publishes into rather than the one it is cut from.** The contract spells
   `onevcs session open REPO [--branch B] [--base B]` and leaves what a pin means to
   inference. It was read as "cut this name from that base", and a pin naming a
   branch that already carried work was refused — so the only way a caller could
   express "continue this work" was to pass the branch as its own base. That session
   then published the branch into itself: `nothing to publish: the base already
   carries this branch's content`, whatever it had committed. It stranded four
   workstreams across three runs on one host, each recovered by hand. So a `--branch`
   naming a branch a checkout of the identity or origin already carries now opens the
   worktree at that branch's tip, `--base` is what the work is merged with and
   published into, and `--branch` equal to `--base` is refused by name. A `--branch`
   naming nothing is unchanged. The three shapes a caller can have written are
   therefore: a pin naming nothing, which behaves exactly as before; a pin naming an
   existing branch, which was a refusal and is now a continuation; and `base ==
   branch`, which was the workaround and is now a refusal naming the spelling that
   replaced it. Confirming this means an amendment saying what the two options mean,
   not a new shape to approve — no public item changed.
13. **`Recoverable` gained two fields, and the contract lays out none of its
   fields.** `held_by` and `net_negative` extend the inferred shape recorded in the
   first table rather than any approved text, and they are what make the row's
   `recover_command` honest: a report whose value is that its output can be trusted
   without checking offered a command that would have published a branch a live
   agent was still writing to. Both are optional and omitted when empty, so nothing
   that reads the old document reads a different one — but a field added to a
   published struct is a constructor break for anyone building a `Recoverable` by
   hand (`onevcs-testing` does, and moved in the same change). Confirming it means
   one amendment naming the two fields and the four types they carry, not a new
   answer to approve.
