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
| `SessionRequest` | `repo`, `branch`, `base`, `execution_checkout` | Exactly the operands and options `onevcs session open` takes. |
| `SessionToken` | newtype over `String` | Opaque by design; the CLI takes and prints it as text. |
| `Provenance` | `complete` / `incomplete-step` | The contract's ported invariant, "dirty adoption -> incomplete-step commit", gives the two cases, and `commit-preserved` carries "provenance kind". |
| `PreservedBranch` | `branch`, `base`, `provenance`, `change_url`, `change_base` | The last two are named explicitly as the host-neutral stack metadata; the first three are what `preserve` must return to be usable. |
| `Scope` | `all` / `repo(String)` | `recoverable` is documented both across every registered identity and for one repository (`onevcs recover BRANCH --repo PATH`). |
| `Recoverable` | `identity`, `branch`, `checkout`, `stopped_because`, `recover_command` | What a "recoverable" view has to answer: where the work is, why its workstream stopped, and the exact command that lands it. |
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
which is what this file is for.

<!-- llmlint: ignore[contracts_have_one_source_or_a_drift_gate] the table below is a
reviewer's record of which lines are approved and which are an inference, not a second
declaration of them: the authoritative one is the amendment in docs/contract.md, and the
suite reconciles that with the types. Gating a rationale column would hold the reasons to
the code rather than the shapes, and the pre-existing rows above have the same character
for the same reason. -->

| Type | Inferred shape | Why |
| --- | --- | --- |
| `SessionRecord` | `session`, `identity`, `lifecycle`, `provenance` | What every command that takes a token needed off the private record and could not derive from a `Session`: which repository it belongs to, whether it is still open, and whether its branch carries an incomplete-step marker. |
| `PublishRequest` | `policy`, `title` | Exactly the options `onevcs publish` takes beyond the token. `title` is a `Subject` rather than a `String`: a publication commits and merges before it composes a message, so the check has to be in the conversion that builds the request rather than where the message is composed. |
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

## One public item the contract does not name, and why it is not an inference

`provenance::SUBJECT_LIMIT` — the length a publication holds a commit subject to,
`120`. The contract names neither the constant nor the module, so this is the one
place in the crate where a public item exists that it does not list, and it is
recorded here rather than passed over.

It is not a shape somebody inferred: the **operator** raised the limit from 72 and
directed that `onepipeline` read it at `onevcs::provenance::SUBJECT_LIMIT` to
validate a plan's titles at load — an hour before a publication would meet the same
rule, which is exactly the interval that twice cost finished, gate-green work. A
consumer that restated the number instead would be a second copy of the rule that
drifts the first time it moves, which is the failure the rest of this file exists to
prevent.

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
8. **`onevcs` stamps none of the envelope's reserved label keys.** It stamps
   `session` and `identity`, which are free-form extras, so a filter naming
   `run_id`, `node`, `step`, `member`, or `persona` admits nothing this crate
   produces — correctly, by the grammar's own rule, and the same answer a consumer
   would get from any producer that did not know the run around it. Whether a
   session should learn them (from its opener, or from the environment a run sets)
   is a question about what a session knows, not about filtering, and it would
   change the bytes of every stream — so it is reported here rather than taken in
   passing.
