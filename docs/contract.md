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
`<prefix>Recovered-Incomplete:`, and `<prefix>Change-Url:`.

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
pub struct PublishRequest { pub policy: Option<MergePolicy>, pub title: Option<Subject> }
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
onevcs publish TOKEN [--policy P] [--title T]
onevcs recover BRANCH --repo PATH | recoverable [--json]
onevcs integrate BRANCH... [--push] | sync [BRANCH]
onevcs events TOKEN [--follow] | artifact cat ID
onevcs rules check REPO
```

`publish` exit: 0 merged or change-open per policy; 1 gate/checks failed (stream names the check, artifacts carry logs); 2 invalid; 3 sync-conflict after bounded resolve-and-requeue.

Event kinds: `session-opened`, `fetch`, `lock-wait` (identity, elapsed, queue_position), `lock-acquired`, `gate-started`, `gate-verdict` (pass/fail, log artifact), `commit-preserved` (provenance kind), `push`, `change-opened` (url, host kind), `change-check` (name, required, status transition, conclusion, log artifact on completion), `change-merged`, `merge-queued`, `merge-completed`, `recovery-attested`, `sync-conflict`, `session-closed`.

Ported invariants: publication checkout never worked in / ff-only; per-run `--shared --no-checkout` clones with gc protection on lenders; flock queueing with fetches outside exclusive sections; every git command bounded by timeout; newest-3 dead-run retention; dirty adoption -> incomplete-step commit; one commit per base advance with `Recovered-Incomplete` trailers; gate environment passthrough (`comparison_env`) so a publishing push replays the worker's judged verdict.
