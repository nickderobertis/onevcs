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
it exists for:

| Type | Inferred shape | Why |
| --- | --- | --- |
| `SessionRecord` | `session`, `identity`, `lifecycle`, `provenance` | What every command that takes a token needed off the private record and could not derive from a `Session`: which repository it belongs to, whether it is still open, and whether its branch carries an incomplete-step marker. |
| `PublishRequest` | `policy`, `title` | Exactly the options `onevcs publish` takes beyond the token. |
| `Publication` | `session`, `branch`, `policy`, `outcome` | What a caller journals about a publication: which session and branch, the policy it was actually taken under (after the rules file and any narrowing), and what happened. |
| `PublishOutcome` | `merged` / `change-open` / `queued` / `nothing-to-publish` / `failed` | The four endings the CLI printed as prose, plus the failure it printed to stderr and reported as an exit code. `Retention` is on the failure because the branch is the only record of the work, and whether it survived is the first thing a caller asks. |

Every type reachable from a supplied implementation's state also gained
`Deserialize` beside its `Serialize` — `Session`, `SessionToken`, `Provenance`,
`PreservedBranch`, `Recoverable`, `Scope`, `SessionRequest`, `ChangeRequest`,
`ChangeId`, `Sha`, `Check`, `ChangeSpec`, and `MergeOutcome`. Reading a state back
is what makes a scenario something a test can write down, and `onepipeline` had
already recorded the two it could not read (`SessionToken`, `MergeOutcome`) as
mirrors waiting to be deleted.

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
