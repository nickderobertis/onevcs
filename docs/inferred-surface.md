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

## Open questions for the planner

These are reported rather than resolved:

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
4. **The provenance trailers are spelled by this crate and named by nothing.** The
   contract requires `Recovered-Incomplete` trailers and an incomplete-step
   commit, but fixes neither key. This build writes `Onevcs-Status: incomplete`,
   `Onevcs-Change-Base:`, and `Onevcs-Recovered-Incomplete:` — host-neutral, and
   prefixed so a repository's own trailers cannot be mistaken for them. A branch
   preserved by ai-orchestrator carries `Orchestrator-`-prefixed trailers instead
   and is *not* recognized; whether onevcs must read those too is a decision for
   the planner, not for this crate.
5. **`Scope::Repo` is reached by where a command is run.** `onevcs recoverable`
   takes no repository operand, and the contract documents the view both across
   every identity and for one repository. Run inside a registered checkout it
   answers for that repository; run anywhere else, for all of them. An explicit
   operand would be a contract amendment.
