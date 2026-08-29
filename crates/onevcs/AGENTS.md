# The crate

Instructions that are true of `crates/onevcs` and nowhere else.

> `CLAUDE.md` beside this file is a symlink to it — edit `AGENTS.md` only.

## The public surface is the contract, and the rest is private

`src/lib.rs` exports exactly what the approved contract names. Everything the
implementation needs beyond that is a private module, so a new seam is added
behind the surface rather than beside it.

## The two interfaces are reached through `Providers`, never named

Nothing outside `providers.rs` names `Git` or `GitHub`. A command takes its
implementations off the `Providers` it was handed, `run` is
`run_with(cli, Providers::real())`, and a publication asks
`context.hosting.for_repo(slug)` for the host it lands a change with. Reaching for
a concrete implementation at a call site is what made both traits decorative for
three releases; `grep 'dyn Vcs'` and `grep 'dyn RemoteHost'` are how you check
they still are not.

The seam covers the session record too, and that is what makes it whole: `Vcs`
owns reading a session, closing it, and publishing it, so a session a supplied
implementation opened is a first-class session in every command that takes one.
It was not always so — the record was written by `Git` directly, and `publish` and
`session close` therefore refused a provider-opened session as a session nobody
opened. Anything that reaches for `workspace::load` from a command is that bug
coming back.

`tests/e2e/seam.rs` holds each command to the implementation it was handed: with a
provider that knows the answer it succeeds, with one that does not it fails, which
cannot happen if the command never asked.

## Both surfaces, one decision

`publish`, `session close`, and reading a session's events each have a typed
library entry point beside the CLI — `crate::publish` answering a `Publication`,
`close_session`, `session`, and `EventStream`. The CLI is a *rendering* of those
and never a second path: `app::publish_session` calls `crate::publish` and turns
the outcome into stdout, stderr, and an exit code. A consumer that had to parse
that stdout is why the value exists, so a failure that the CLI reports as a
non-zero exit is a `PublishOutcome::Failed` rather than an `Err` — the two
surfaces cannot disagree about which failures are which. `tests/e2e/library.rs`
drives every one of them twice, on the providers and on real `Git` + `gh`.

Reading events takes a filter on both surfaces — `EventStream::open_filtered` and
`onevcs events --filter` — and the grammar is **shared with `oneagentgraph` and
`onepipeline`**, fixed across the three. Do not extend it here: a field one of
them understands and the others do not is a consumer's filter meaning three
different things. Two rules follow from what filtering is for. A filter decides
which events a consumer *wants*, never which lines of a file are worth reading, so
it is applied after the refusals — a stream that is not what a writer left is
refused whichever events were asked for. Both readers of a stream's *values* go
through `stream::attributed`, which is where those two refusals live, so `--filter`
and `EventStream` cannot come to differ about which line is unreadable or whose
event it is. And the command prints the producer's own line rather than a
re-serialization of what it parsed, so a filtered read is a subset of an unfiltered
one byte for byte.

## An event names its phase, and a session read is scoped by it

Every envelope carries the `phase` of a change's life its producer stamped it with,
and the filter grammar matches it. Four things about that are easy to undo.

- **The producer stamps it, and `push` is the reason there is a producer at all.**
  `Phase::of` answers for sixteen kinds; a push's phase is a fact about the branch it
  updated, so `record_push` takes one from its caller — `Phase::Development` for the
  session's own branch, `Phase::Integrate` for the base a `local-direct` squash lands
  on and the base a merge train advanced. `Stream::emit_push` is the only way to emit
  one.
- **The field is additive inside `v: 1`, and stays additive.** An envelope written
  before it existed reads at the phase its kind decides (`StoredEnvelope` in
  `event.rs`), and a build that predates it reads one carrying it — which `compat/`
  proves against a released `onevcs` from the registry rather than against this
  build's own reader.
- **Which phases a session *has* is derived, never configured.** `stream::supported`
  answers it from the session record, the resolved merge policy, and whether
  `$ONEVCS_HOME/releases.yml` configures targets. Every way it can fail to reach an
  answer **widens** the set, because a read that quietly left events out is
  indistinguishable from a session that never wrote them. Naming an unsupported phase
  is refused where it is named; naming none drops it in silence.
- **The release correlation is a join `onevcs` makes so a consumer never has to.**
  Where the release phase is supported, `EventStream` also hands back that identity's
  `release-observed` and `release-acknowledged` events whose `landing_commit` is this
  session's own landing, at the producer's own `stream` and `seq`. The address of
  that stream is spelled once (`stream::releases_token`) and never rendered — a
  refusal about one of its lines names the *identity*. `release-probed` is not
  correlated: the session's own stream already carries the probes its publication
  ran. The set grows after `session-closed`, so the reader re-reads that file rather
  than advancing a cursor over it — a landing commit becomes knowable long after the
  events were written. Which is also why the *landing* question is asked on every read
  that has an un-handed candidate, rather than only when that record has moved: the two
  halves become true independently, so a reader that remembered "these did not match"
  would answer from the last time the record moved and drop a release recorded while
  the landing could not yet be read.

## A branch outlives its session, so a retried session says who continued it

`workspace::Record::retried_by` names the session that continued this one's branch,
written onto the *older* record when the newer one opens (`workspace::supersede`).
Everything that answers what became of a session or a branch — `status` in each of
its four spellings, `release status`, and `recoverable` — follows that chain to its
newest record before it decides anything, and a superseded session's clone is
reported as holding the branch and excluded from judging it.

Three things are load-bearing. A link is refused where it is **written**
(`workspace::followable`, called from `save`) if its target is missing, belongs to
another identity, or closes a cycle. A chain that is nevertheless unfollowable
answers `unknown` and says why — never the last record that still read, and never a
decided `no`, because a wrong `no` here is a paste-ready publication of work the base
already carries. And the record keeps the keys this build has no opinion on across
that rewrite (`remainder.rs`), because recording a retry is a read-modify-write of a
document a newer `onevcs` may have written.

## Three verbs land a branch, and provenance is what chooses between them

`publish` takes a session token; `recover` and `publish-branch` take a branch name
and are **one path** (`branch.rs`), because a second locate, clone, or base-merge
beside it is drift nothing would catch. What separates them is provenance and
nothing else: `recover` requires an unattested incomplete marker and writes the
attestation that clears it, `publish-branch` requires that there is none.
`integrate` stays the local-only merge train and routes to `publish-branch` rather
than re-verifying a branch itself; what verifies a train is the `pre-push` hook at
the push that publishes the advanced base.

Which means **a refusal on this path is the guidance surface**: each one names the
command with its exact arguments, or the rules-file entry, that resolves it. A
refusal that only diagnoses leaves an agent to invent a way forward, and what it
invents is `git push` plus `gh pr create` — the thing this crate exists to make
unnecessary.

**Syncing a branch has two shapes, and its record decides which** (`publish::reconcile`,
used by all three verbs). Ordinarily the base is merged into the branch. A branch stacked
on the change below it belongs on the root once that change lands, so it replays only its
own commits there and retargets — merging instead would replay the change below against
whatever the base holds of it.

**What makes a publication stacked is its record, never its content.** A branch carrying
the change below it reads exactly like a branch that wrote those commits itself, so a
stack inferred from content rewrites branches nobody stacked. Recorded stack state that
cannot be read is refused, naming what restores it: publishing around it would answer
"no stack" from a value nothing could read, which is the merge all of this avoids.

**A name in two checkouts is two copies, and they are compared rather than ordered**
(`branch::locate`). `workspace::checkouts_of` is a search order, not a preference: one
copy has to carry all the others or the landing is refused, because publishing the first
tier that has the name publishes a stale copy. Every copy is in that comparison,
including one whose content the base already carries — its *content* is spent, but its
commit is a commit like the rest, and one nothing descends from is a divergence whatever
became of what it held. Only where every copy is spent is nothing compared: the answer is
that there is nothing to publish, from the first in search order. Whichever way it goes,
the answer names every checkout holding the name — a stale selection and a current one
read identically otherwise.

**And the refusal says how the two copies differ.** A refusal is terminal for an
unattended run, so one that only says the copies disagree leaves a person diffing
checkouts by hand. `branch::diverged` names, per copy, its checkout, tip, parent,
subject, tree, and commit date; says whether the pair is amend-shaped (one parent and
one subject over two trees) or two separate commits; and prints the fetch and the diff
between the two commits. Each commit is read out of the checkout that holds it, so the
answer never depends on one checkout being able to see the other's objects. What it must
not do is choose: publishing either copy loses the other.

**A copy nothing descends from is refused rather than superseded.** Selecting it would
guess that a rewrite supersedes what it rewrote. The workflow that reaches this — a
replay from where `sync_change_base` sends an operator — is recorded as a journey rather
than described here.

`recoverable` is the report `recover` and `publish-branch` are reached from, so
the command it prints per row is one of them, by path (`--repo`) rather than by
cwd. The train is deliberately not what it names, even for finished work:
`integrate` reads its candidates out of the publication checkout alone and
refuses a team or remote identity outright, so it lands none of the branches this
report is most often read about — the ones a run left in its own clone.

## The repository's own merge path is the only verifier

Nothing here runs a verification tier of its own, and the rules file names none.
For a remote-first identity the verifier is the host's required checks; for a
local-first one it is the `pre-push` hook git runs at the publishing push. Three
things follow, and each is easy to undo by accident.

- **This crate hands the merge path what it needs and keeps what it wrote, and does
  nothing else about verification.** `merge_path::comparison_env` exports the remote
  and base every judging process resolves — a hook left to find its own resolves the
  repository default, which for a stacked change is not the base being published
  onto — and `merge_path::preserve_log` keeps a push's output where it outlives the
  tree it was built in.
- **A `push` event is a verdict.** `accepted` is what the merge path ruled and the
  artifact is what the hook wrote, for every publishing push whatever the outcome.
  `status` reads its `merge_path` section off that event and nothing else.
- **`recover` refuses to attest a branch nothing verified**, asking
  `store::merge_path_coverage` — the same question `onevcs register` warns on and
  `onevcs repos --audit-gates` reports. Those three must keep one answer.

## A publication observes, captures, and does not settle early

- **Polling is driven by `context.effective`, and by nothing else.** What a policy
  *calls* its verification never decides what a publication observes: a field read
  here for that once left the host's required checks observed for no repository at
  all.
- **Every capture is best effort where the thing it records has already happened.**
  `record_push`, `report_conflict`, and `record_landing` warn on stderr and carry
  on. A `?` in any of them turns a push git accepted, or a merge the host performed,
  into a publication that failed — and sends somebody to land work that is landed.
- **A conflict's paths and hunks exist only before the abort**, so `git::conflict_in`
  takes both in the same pass that decides it *was* a conflict, and reads them
  NUL-delimited: git quotes a pathname carrying a newline or a quote in its default
  listing, and a reader that unquoted nothing would name a file nobody has.

## The subject policy is the repository's, and this crate holds none

`publish::subject_for` composes the subject a publication lands under — an explicit
`--title`, otherwise the most significant commit subject on the branch — and then
puts it to the target repository's own `commit-msg` hook through
`git::message_policy`. A rejection refuses the publication and hands back what the
hook wrote; a repository with no executable `commit-msg` hook is asked nothing and
told nothing.

Three things about that are deliberate and easy to undo by accident.

- **No conventional-commit knowledge lives here, and none may.** Which types cut a
  release is a fact about the repository, stated in its own hook. A type list, a
  subject grammar, or a "does this parse as conventional" check added to this crate
  would be `onevcs` acquiring a policy — and a check that a title *parses* is not
  the check anybody wants: `docs:` and `chore(deps):` parse, and in a repository
  where neither releases they merge green and reach no registry.
- **This is the only point where the check can happen at all.** A squash-merge
  subject comes from the change request's title, not from a commit anybody wrote
  locally, so no local hook ever sees it. Which is also why the hook is asked
  wherever the subject is *composed* — `subject_for` — rather than beside a `git
  commit`, and why `branch.rs`'s precondition and the publication ask through the
  one function.
- **A hook that could not run is not a hook that said yes.** `git::message_policy`
  answers `Unstated` only for a hook git itself would skip — absent, or not
  executable — and everything else is either a verdict or an `Err`. A rejection is
  `Error::GateFailed` (exit 1, the repository turned the subject down); a hook that
  could not be executed is `Error::Invalid` (exit 2, nobody answered). Discovery is
  `git::hooks_dir`, so `core.hooksPath` is honoured, and the bound is the
  hook-running one every other hook in `git.rs` runs under.

A repository states its policy by carrying the hook; one that carries none is left
exactly as it was. Nothing here needs a hook to exist anywhere for the crate to be
correct, and the absent-hook journey is what holds that.

## `status` and `import`, and the three things easy to undo

- **A landing is decided from the base's own history, never from content alone**
  (`landed.rs`, asked by both `status` and `vcs::collect` so the two cannot disagree
  about one branch). Four tiers, most certain first, and the answer names the one that
  decided it; the comparison of content is last and never answers `yes`. Three answers
  rather than two, because calling an undecidable landing `no` is what puts a
  paste-ready `publish-branch` under work the base already carries. What `status`
  adds over `recoverable` is the *exclusion reason*, stated with the tier that
  decided it.
- **A change request's URL resolves only through the event stream**, because nothing
  on a branch carries it: `status URL` cannot answer for a change something else
  opened, and widening that is a contract amendment.
- **`import` writes refs and nothing else** — into a scratch ref, judged there, then
  the destination's ref. A name the destination has *checked out* is refused rather
  than written, and a non-fast-forward is refused naming the commits that would go,
  because `--as` is the way through and only works once those are visible.
- **`status --json` is a versioned object whose bytes are checked in.** It declares
  `version`, an absent field is omitted rather than written as `null`, and the
  goldens under `tests/golden/` are compared byte for byte against the real CLI.
  Changing what the object carries means bumping `status::REPORT_VERSION` — which is
  the one place the number is stated — and re-making both goldens, named for that
  version, in the same change.

Both find a branch nobody named through `branch::locate`, which is the search the
two publishing verbs use.

## What a report answers about, and what a name already means

Preserved work goes missing through silence rather than through a search that
could not reach it, so what a report has withheld, what scope it answered under,
and what a name already means are each stated rather than left to be inferred.

- **A row is a command only when the branch is ready for it.** `recoverable` is read
  to be trusted without checking, so a branch a live session still holds withholds its
  command rather than annotating it — and so does one whose work reached the base,
  which only `--all` lists and which carries an empty `recover_command`. A branch
  nothing can decide about is listed rather than withheld, because it may be work
  nobody published; it keeps the argv and loses the label that reads as "paste this".
  Live is asked two ways — the process that opened
  the session, and its run root's occupancy lease — because each is the true one at a
  different time. A net-negative branch is marked instead of withheld: stripping work
  may be exactly right, and this report does not decide. Both are measured against the
  base the branch would be published into, so this report and that publication cannot
  disagree about what it would land.
- **A scoped answer names its scope.** `recoverable` answers for one identity when it
  is run inside a registered checkout and for every identity when it is not, and nobody
  types which — the directory decides. Unsaid, a scoped answer reads as the whole
  host's, so every rendering names it.
- **A branch pin that names something is continued, never cut fresh over it.** A name a
  repository of the identity carries *means* the work on it, and cutting a branch over
  it produces an empty second branch of that name which cannot even be handed back. So
  every repository the identity keeps branches in — the run clones included — and
  origin's own copy are asked before a name is treated as new. Which copy is continued
  is a comparison one has to win: one must carry the other or the session is refused,
  because taking either otherwise opens on a tree that drops the commits of the one
  passed over.
- **…which makes `base` the integration target and not the starting point.** For a
  continued branch it is only what the work is merged with and published into, through
  the same reconciliation every landing syncs with; a conflict refuses the session
  rather than leaving one in a worktree nobody asked to resolve, and a branch named as
  its own base is refused, because it would publish the branch into itself.
- **…and a pin an *open* session already holds is that session, resumed** — the same
  base, the same execution checkout, and a run root that is there and free. Closed is
  not one of them, because closing hands the branch back and means finished.

## Tests are journeys, and the four unit tests say why they are not

Behaviour is a journey. `tests/contract.rs` holds the approved surface to the
contract text it is extracted from; everything else in `tests/e2e/` spawns the
compiled binary and drives it against real git. A path only an in-process test
could reach is a path to *delete*, not one to unit-test — which is also how the
95% coverage floor is met.

The `#[cfg(test)]` modules in `src/` are the exceptions to that, and each one is
there because what it holds is reachable no other way: a process's creation
identity (`workspace.rs`), a reader overlapping an atomic replace (`home.rs`),
Windows' verbatim paths crossing every git boundary and a captured command's
collector meeting its pipe empty at the instant the command exits (`git.rs`), and
the *type* side of the status report's serialized contract (`status.rs`). The
collector's is the one that looks like a journey and cannot be: what it holds is
an *interleaving* — a reader taking a read that finds the pipe empty, and finding
its command already collected when it next looks — and on an idle host that window
is nanoseconds wide, so it is arranged rather than waited for.
`tests/e2e/inherited_pipes.rs` drives the same collector through the binary and
asserts on what a caller is shown, which is the half no injected pipe can prove.
The status one is the one to be careful with, because it looks like it belongs
outside: the report's types are deliberately private, so proving the checked-in
goldens read back as reports from `tests/` would mean making a dozen types public
for a test's benefit. Both halves read the same two files — the CLI's bytes there,
the type's round trip here — so neither can drift from the other.

`tests/e2e/honesty.rs`, `tests/e2e/seam.rs`, and `tests/e2e/library.rs` are the
modules that do not spawn the binary, and the reason is the thing they test: the
library surface — `run_with`, and the typed entry points beside it — is reached by
supplying implementations, which the binary deliberately has no way to do, so a
journey about it can only be in-process. `honesty.rs` runs one publication and one
session journey twice — `Git` + `GitHub` against the providers in
`crates/onevcs-testing` — and holds the two event streams to each other. That
comparison is what keeps every consumer's suite honest, so a provider that stops
matching fails here rather than downstream. All three write `ONEVCS_HOME` and
friends into their own process, which is safe only because `cargo nextest` gives
each test its own process; `cargo test` would race them.

`tests/smoke/` is the exception to "no unit tests" being the whole story: it is a
second test binary, excluded from `just test` and `just gate` by name, that drives
both interfaces against **real** git, a real GitHub remote, and the real API. It is
in-process for the same reason the three modules above are — what it holds is the
interfaces, and `Vcs::preserve` and every direct `RemoteHost` call are reachable no
other way. `just smoke-real` runs it. It never skips and never substitutes `gh`: a
missing credential or a repository whose name does not end in `-smoke` is a loud
failure, because a smoke that can pass without talking to GitHub proves nothing.
One module there needs no credential and touches no scratch repository —
`tests/smoke/releases.rs`, which drives `scripts/release-probe.sh` against the
public registries this repository publishes to. It is in this binary because this
is the tier allowed to reach the network at all, and a check that quietly needed a
registry would stop `just check` being offline.

**A Linux host running this suite needs `fuse3`**, because one journey mounts a
filesystem of its own to make a real removal fail. `just bootstrap` provisions the
package; without it that journey refuses rather than skips, since one that passed
without building its own premise would prove nothing.

`tests/e2e/world.rs` is the fixture, and it is Unix-only: the program it installs
as `gh` and the `pre-push` hooks the verification journeys write are POSIX shell, and a
fired timeout takes a process *group*, which has no portable spelling. Windows CI
builds the crate and runs the contract, boundary, packaging, and inherited-pipe
suites.

`tests/e2e/inherited_pipes.rs` carries a fixture of its own rather than
`world.rs`'s, because it has to run where `world.rs` cannot. It holds one rule in
both directions: **a command's collected output begins and ends with that command**,
so a run must not wait on a pipe the command no longer owns, and a caller must never
be shown bytes an unrelated process put in that pipe afterwards. The second half is
the one that reads as harmless and is not — on the stream carrying a probe's answer,
one added line is not a stray byte, it is the loss of the version the probe wrote.
Three things about the module are the point rather than an implementation detail.
Its unrelated process — the one still owning the write end of a finished command's
pipe — is launched by the journey and never by the command under test, because a
descendant is killed by the very teardown it exists to outlive. Its stand-in `git`
and its script probe are one `std`-only program compiled by `rustc` at journey time
(`tests/e2e/programs/pipe_holder.rs`), because both have to be executable on every
host and a shell script is not; that stand-in runs the real git, on the same pipe.
And it is Linux and Windows only: taking a duplicate of another process's pipe is
`/proc/<pid>/fd/1` on one and `DuplicateHandle` on the other, and macOS offers an
unrelated process neither.

**A collection ends on the command's exit, and every wait under it waits on a
thing rather than on a clock.** Those are two rules and the second is the one that
gets dropped, because breaking it costs no test. `WouldBlock` on a non-blocking
pipe reports that nothing is readable *at this instant*, which is equally what a
command mid-write looks like — so the reader asks whether its command is over
*before* the read that answer decides, never after, and a build that asked
afterwards discarded whatever arrived in between. And a collection that sleeps a
fixed span between looks charges every captured command that granularity: a
reader waits on its pipe becoming readable, end of stream retires one at once, and
the run waits on a reader ending — which a child's exit brings about by closing
the write ends it owns — rather than on a tick. A build that polled at 10ms
instead ran a workload of 240 small git commands in 6.9s against 2.2s, and this
crate runs many per node.

**A fixture that selects a repository by `path:` must spell it the way the registry
holds it, which is not the way the fixture built it.** `policy::matches` compares a
rule's `path:` literally against the registered checkout, and that is
`canonicalize`'s answer — on Windows the verbatim `\\?\` namespace, which no plain
path equals. So ask the binary (`onevcs resolve` answers `publication_checkout`)
rather than composing the path. A fixture that composed one matched no repository on
Windows and only there, which is not how it reads: no release target existed, the
command refused before running anything, and the three probe-driven inherited-pipe
journeys waited out their whole bound and then blamed the stand-in for not
publishing a pipe nobody had asked it for. `just gate` runs on Linux, where the two
spellings are the same string, so this is another class of defect only `cross` sees
— and it is why `published` in that module takes the running command and refuses to
keep waiting on one that has already ended.

Two journeys go one step narrower and skip on Apple platforms: the ones about a path
listing this process cannot decode. Their premise is a filename that is not UTF-8,
and only a filesystem storing a name as the bytes it was handed will hold one — git
prints a repository's own path bytes, and `-z` turns off the quoting that would render
them as ASCII, so no listing can be made undecodable from a name that decodes. Apple's
filesystems enforce UTF-8 and refuse the name with `EILSEQ`. It is the fixture that is
unportable and not the behaviour, so those two carry `#[cfg_attr(target_vendor =
"apple", ignore = ...)]` — a skip nextest counts, rather than a test that passes
without having built its premise. `just gate` runs on Linux only, so this is a class of
defect only the `cross` job sees.

## The tier that talks to GitHub

Every other journey here is offline, and the cost of that was measured rather than
hypothetical: `GitHub::change_checks` asked `gh pr view` for an `isRequired` field
that command has never returned, and `GitHub::check_log` asked `gh run view --job`
for a job by a check's *name* when it only ever accepted a job id. Both shipped
green for every release, because the only thing that had ever read them was a shell
script written beside them that answered to what they asked.

- **The scratch repository is `nickderobertis/onevcs-smoke`**, and a repository
  whose name does not end in `-smoke` is refused before the first mutating call.
  `ONEVCS_SMOKE_REPO` names a different one; it must clear the same rule.
- **A whole run is about a minute and under a hundred API calls** — measured twice
  on a warm build, 58s and 65s wall clock, three pull requests opened and merged
  each time. The call count varies with how long the real Actions job takes to
  settle, because the checks journey polls a job it cannot hurry: 7 REST + 44
  GraphQL on the faster run, 29 REST + 54 GraphQL on the slower. Take the larger
  pair as the budget. That is the number to weigh when deciding whether to make
  `smoke` a required check, and the reason a journey should cover several methods
  rather than one. The checks journey then reads the same change request a second
  way, through the Actions API — a handful more REST calls, and the only way that
  path is proved from a machine whose credential would always take the other one.
- **A run is uniquely named** by journey label, process id, and its own scratch
  directory, so two runs at once cannot collide on a branch or a change request.
  Cleanup is a `Drop`, so a run that fails half way still removes its branch; what
  it can leave behind is a merged (or, on a failure between opening and merging,
  an open) pull request, which is deliberate — that is the evidence it ran.
- **The scratch repository declares no required check.** Its `change-direct`
  journeys still consult the host's checks — every automated policy does — and a
  host that declares none has *answered*, so they proceed and the merge is the
  host's own to refuse. What cannot be proved there is the waiting: a publication
  that ends at a check going green stays covered by the offline tier, and this tier
  proves `change_checks`, `check_log`, and `merged_at` themselves against the real
  workflow the repository carries. Making its check required would need branch
  protection, which would then also gate every merge this tier depends on.
- **The honesty comparison has two legs.** `tests/e2e/honesty.rs` is the offline
  one and runs in every gate; `tests/smoke/honesty.rs` is the same comparison with
  real `Git` + real `GitHub`. Both reduce their streams with
  `tests/e2e/comparison.rs`, so neither can accept a difference the other rejects.
- **The credential is part of the backend, so ask `gh` only for what you read.**
  GitHub declines a field a token may not see by failing the whole `gh` call, so a
  query carrying one unreadable field takes down every caller of it — and only once
  there is something to refuse, which makes it look intermittent. Two rules follow:
  each call names the fields its caller reads (`tests/e2e/host.rs` holds it there),
  and a refusal stays a refusal — reading one as "no checks" is what lets a merge
  through. Granting the permission is the operator's move, not the parser's.
- **A fine-grained token can never read a check run, so the checks are read from
  GitHub Actions.** Not a scope to widen: GitHub offers no `Checks` permission for
  that credential class, so `gh pr view --json statusCheckRollup` and `gh pr checks`
  are both out of reach for `RELEASE_PLZ_TOKEN` and for anyone who authenticates
  `onevcs` the way GitHub recommends. `change_checks` and `check_log` fall back to
  the Actions API (`Actions: Read`), which reports one check per workflow job and
  addresses its log by job id, and the answer carries `sources` so a caller can tell
  Actions-only visibility from the whole picture. Three rules go with it: the
  fallback is taken **only** on a permission refusal, because answering from the
  narrower source whenever the complete one merely went wrong would drop whatever a
  third-party integration posted; which checks *block* comes from the repository's
  rulesets there, which do not report classic branch protection, so a
  classically-protected repository under such a token waits for a required check
  that never arrives rather than merging; and a credential that can read neither
  source is an error naming both refusals and the permission, never an empty list.
  `ONEVCS_CHECK_SOURCE` narrows the choice for an operator who already knows what
  their token can read, which is also how `tests/smoke/checks.rs` proves the Actions
  path from a machine whose `gh auth` would otherwise never take it.
- **Which endpoints that path may reach is an assertion, not a description.** The
  claim is that `Actions: Read` suffices, and that claim is exactly the claim that
  every call is one such a token may make — which no answer can show, because a
  developer's own credential reads the rollup happily and every stand-in answers
  whatever it is asked. `CHECK_ENDPOINTS` in `tests/e2e/host.rs` is the list, and
  the journey beside it drives a whole publication and asserts over the calls the
  substituted host recorded: the set of `gh api` paths reached is exactly that list,
  and no call names `statusCheckRollup`, `pr checks`, or `run view`. Add an endpoint
  to the read and add it there.
- **A call whose output is content goes through `gh::invoke_content`, and a log
  that did not arrive is never an artifact.** `gh` will not print content carrying
  terminal escape sequences unless asked, and a CI job's log almost always carries
  colour, so both calls that fetch one pass `--allow-escape-sequences` and fall back
  once a `gh` predating the flag rejects it — that rejection comes while `gh` parses
  its arguments, so either generation costs one request. What arrives is stored
  unaltered; rendering an artifact safely belongs to whatever prints one. And a
  fetch that failed is an error, never an artifact holding the reason: an artifact
  reads as what the check printed. `publish` says so on stderr and records the check
  without its log rather than failing, because the log is evidence and `conclusion`
  is what decided the merge.
- **A check's name is matched against the host's own names, never passed to `gh`.**
  Both log calls find the job by matching it — against `gh pr checks`'s `name` field,
  or against the job names the Actions API lists — and then ask for the log by the
  job's *id*. So `addressable`, which guards values that really do enter the argument
  vector, is not what a check's name is checked by: it refused whitespace, and every
  GitHub matrix job is named `check (macos-latest)`, so every matrix check on every
  change request was recorded with no log while the warning blamed the host. Which is
  the second half: this build refusing a name and the host declining to produce a log
  are two answers and read as two, because the next move differs.

## Releases are what follows a landing, and two distinctions carry the whole thing

`releases.rs` is the document, `probe.rs` runs a probe, and `release.rs` is
everything else. Six things are easy to undo by accident.

- **The registry is not part of this feature.** The releases document is at
  `$ONEVCS_HOME/releases.yml` and nowhere else, no release verb reads or writes the
  registry, and its version did not move. Adding a key there is the reflex to
  resist: builds already in the field declare `deny_unknown_fields`, so the first
  host to configure a target would stop every one of them.

- **Every state-root document is read leniently, and written back whole.** The
  registry, the rules file, the releases document, and the release record each
  accept a version above the newest this build knows and keys it has no opinion on;
  `remainder.rs` hands those keys back when a verb rewrites the document, and a
  write never lowers a declared version. Still refused: a version below the oldest
  readable one, a required field (named), and a key refused *by name* — the rules
  file's `gate:` at version 3. `tests/e2e/state_root.rs` is the other half: it scans
  for a spawn reaching the binary without a scratch state root, because leniency
  makes a stray invocation harmless and only the guard stops there being one.

- **The style is the shape, not a label.** A probe lives on `ReleaseMethod::Automated`
  and nowhere else, so `ReleaseTarget::probe()` answers `None` for a human-step target
  by construction. `release::ask` takes the `&Probe` its caller already found rather
  than reading one off the target, so there is no spelling of that call that could
  start a subprocess for a target with nothing to run — and the absence of a
  `release-probed` event is what a journey checks that by.
- **"Not answered" is not "not released".** A timeout, a non-zero exit, a spawn
  failure, and output that is not one usable line all answer the first. A consumer
  holds indefinitely on it and acts on the second, so nothing may collapse them —
  not the library answer, not the rendering, not the event payload.
- **An unestablished baseline is not a baseline, and waiting cannot make it one.**
  A probe that did not answer at a landing left this crate not knowing what was out
  *then*; a probe answering a version later cannot repair that, because the release
  carrying this very change may already be in it. Exactly one later answer does:
  `NoRelease`. A landing this crate never probed at all — a target declared after it
  — is the same state and is answered the same way.
- **A probe's output is untrusted data.** It is parsed, quoted into a message with
  `{:?}`, and carried as a JSON string; it reaches no shell and no template. The one
  place a probe *command* is a command is the `sh -c` line an operator configured.

Baselines are captured by the publication, on its own stream, and best effort like
the landing record beside them: the change has already merged, and failing it over
a footnote would send somebody to land work that is landed.

## Two release documents, and neither is the other

`releases.rs` and `release.rs` are the **host's** half: `$ONEVCS_HOME/releases.yml`,
which says what this host waits on, per repository. `declaration.rs` is the
**producer's** half: the `release-targets.toml` a repository carries at its own root,
which says what that repository publishes. The canonical schema for the second is
`docs/contract.md`'s amendment, and six repositories write against that text and
nothing else — so it is the one place the schema is stated, and a field added to the
type without being added there is a field five other repositories will never write.

Three things are easy to undo.

- **They are two formats on purpose.** A repository declares what it publishes and a
  host declares what it waits on; a host waits on a target nobody has published yet,
  and a repository publishes things no host waits on, so reconciling the two into one
  format makes one of those facts unstateable. `the_two_release_documents_stay_two_formats_and_the_contract_says_why`
  in `tests/contract.rs` is what keeps the argument in the file.
- **Deciding between them is *only* in `release::resolve`.** Nothing in
  `declaration.rs` reads the host document, and `release.rs` reads a producer's in one
  function and nowhere else — see the three layers below. A second call site that
  consulted both would answer the precedence question by accident.
- **A short name is `TargetName` and not a second name type.** It is what the host
  document calls a target, what a `--target` operand takes, what a release record is
  keyed by, and what a consumer's plan names — one vocabulary, validated in one
  conversion. The registry-qualified `id` beside it is a different thing and cannot
  stand in for it: one repository publishes both `pypi:onejudge-cli` and `pypi:onejudge`.

Reading is lenient above `SCHEMA_VERSION` and strict at it: a later schema's keys are
ignored, and an unrecognized key at the version this build knows is refused **by name**,
because the likeliest defect in a hand-written file is a typo and reading `manifset` as
an absent `manifest` publishes an answer nobody declared. And a rendering answers the
declaration alone — a producer's comments were never read, so writing one back over
their file deletes the reasoning that is the most valuable thing in it.

## A repository's targets come from three layers, and `resolve` is the only place

`release::for_repository` is where the producer's declaration and the host document
meet, and `release::resolve` is the whole of the precedence: the producer's targets in
its own publication order, a host target the producer does not name appended, and a
host target it does name replacing it *in the producer's position*. The order is stated
in `docs/contract.md` and held there by
`the_precedence_among_the_three_layers_is_stated_rather_than_left_to_read_order`;
`tests/e2e/discovery.rs` drives every ordering through the binary and the library.
Five things are easy to undo.

- **Where a declaration is read from is the same question as where a script probe
  runs**, so it is the same answer: `probe::Checkout`, the registered publication
  checkout on its base. A declaration read off the branch a dispatch is authoring is a
  declaration that dispatch can rewrite. Asking a second way would let the two drift.
- **Every reason there is no such checkout is `Unreadable`, never `Undeclared`.** A
  repository whose declaration this build could not read must not answer as a
  repository with no targets: a consumer that read "no targets" from a document that
  failed to parse stops waiting for a release that is coming. That is the same
  distinction `NotAnswered` keeps against `NoRelease`, at the layer where targets are
  discovered rather than probed, and it is why `DeclarationSource` has three variants
  rather than being an `Option`. Every refusal about a target that is not there says
  so — `RepositoryReleases::unknown` is that clause, and it is empty in both states
  where the answer is complete.
- **An override replaces a target whole and never merges its fields.** A target is
  `{name, style, body}`, and a half-host half-producer one is a probe nobody wrote.
- **A declared target's probe is the declaration's own `probe` given that target's
  `id`** — one script, one registry-qualified identifier, one answer, which is the
  contract the canonical schema fixes. A declaration naming no probe leaves nothing to
  run, so its targets are human steps whose action names `release acknowledge`; the
  absence of a `release-probed` event is what a journey proves that by.
- **`default_target` stopped being decidable from the document alone**, because a host
  naming the producer's `crate` and declaring nothing itself is correct. So
  `honours_default` is asked twice through one function: at load for a rule that says
  `declaration: ignore` — where the rule *is* the whole answer — and of every
  resolved repository. Restoring the load-time check for every rule would refuse
  correct documents.

## The disk is a resource, and one retention rule frees it

Every branch-keyed landing cuts a run root, and `sweep.rs` holds the only rule that
removes one. It is asked two ways and they are the same judgement: deliberately, as
`onevcs sweep` over every family, and by `sweep::enforce` from `branch::prepare`, as
a landing cuts the next run root under its own family. The second is what makes it a
*rule* rather than a chore — nothing else runs between two landings on a host that
publishes all day. A pass that could not run is a warning on stderr and never a
refused landing: what it reclaims is the *previous* runs' leftovers, and losing a
publication to those is the failure the rule exists to prevent.

Six rules govern how it decides.

- **Proof, never inference.** A workspace is removed only where this crate can show
  it is finished. Every other answer retains and reports why.
- **A question that could not be finished is not an answer.** Whether emptying a
  workspace is this host's to do is asked by writing into every directory the removal
  would have to empty; a probe that could not be undone proves nothing and retains,
  and none of them may be left looking freshly written — the age floor reads that
  clock on the next run.
- **A landing holds its own run root's lease**, and it is the lease `recoverable`
  reads, through the same function, so the two cannot come to disagree about who is
  inside a workspace. Nothing a lease is held on is removed, and nothing inside it is
  signalled.
- **The evidence outlives the failure by the age floor**, which is
  `sweep::DEFAULT_MIN_AGE_HOURS` where nobody says otherwise. The merge path's
  preserved logs live under the run root — which outlives the worktree the
  publication was built in — so reclamation is the only thing that takes them.
  Work no origin has keeps its workspace past the floor as well, bounded by
  `workspace::RETAINED_DEAD_RUNS` itself: one bound on one question, asked of the
  lifecycle clones there and of the landings' workspaces here.
- **What "no origin has" means is `vcs::collect`'s question and not a second one**,
  so the report that offers work for recovery and the rule that keeps its workspace
  cannot come to disagree. Publication squashes, which is why neither half of it can
  be dropped.
- **Reclaiming a workspace stops what it left running** (`processes.rs`), because
  unlinking files a live process holds open frees none of their blocks — a
  publication runs the repository's own verification, and verifications start daemons
  that outlive it.
  Nothing live is signalled, and neither is this process or anything it descends
  from: an operator who ran a sweep from inside a workspace is not a daemon. A
  workspace whose holders would not stop is kept and reported rather than
  half-emptied.

The flag surface is shared with `oneagentgraph sweep`, spelling for spelling and
default for default; neither side may amend it alone.

**A session's own run roots are reaped by `workspace::reclaim`, and the occupancy
lease is not what proves one abandoned.** The lease is per command and outlives
none of them — `open` takes it shared and drops it as it returns — so a dispatch
working in its worktree for hours holds no lease at all, and reading an exclusive
take as *nothing is working in here* deleted three live dispatches inside ninety
seconds of their launch. So `reclaim` asks the session records first, exactly the
way `vcs::held_by` asks them of a branch: an open record whose owner is that same
process, still running, is never a candidate, whatever its clone has committed and
whoever holds the lease. The lease is asked afterwards and still answers for the
command that is inside a run root *now*, which is the state a record does not
cover; a run root no record names has only that answer, and is reclaimable, since
protecting it would make a refused open's litter permanent.

## Everything durable lives under one state root

`ONEVCS_HOME` (otherwise `~/.onevcs`) holds the registry document, the advisory
locks and merge-queue state, the per-session workspaces, the conventional
`rules.yml`, and the event streams with their artifacts. A journey points it at a
scratch directory, which is what lets the suite drive the real binary without
touching an operator's own state.

The other environment seams exist for the same reason and nothing else: `ONEVCS_GH`
names the program that answers as `gh`, `ONEVCS_CHECK_SOURCE` narrows which of the
host's check sources may be consulted (`auto`, `status-checks`, `actions`), and the
bounds
(`ONEVCS_GIT_TIMEOUT`, `ONEVCS_GIT_HOOK_TIMEOUT`, `ONEVCS_LOCK_TIMEOUT_SECONDS`,
`ONEVCS_CHECKS_TIMEOUT_SECONDS`, `ONEVCS_CHECKS_POLL_SECONDS`) are operator knobs a
journey turns down so a bound can be *proved* rather than waited out.

## Four rules that are easy to break quietly

- **git's own version is not pinned, so behaviour that varies by it is a bug.**
  CI runs a git years newer than most workstations, and it does more on its own
  than an older one: from 2.49 a plain `fetch` recreates a missing
  `origin/HEAD`. Any answer this crate derives must come from what it asked git
  for, never from housekeeping a newer git happens to perform — otherwise the
  journey that pins it passes on one machine and fails on the other.
- **Emitting an event cannot fail a command.** The stream is the record of what
  happened, and a publication that reached its base is not undone by the record of
  it failing to be written. A failed write says so on stderr.
- **A caller-supplied identifier is checked before it names a file.** A session
  token and an artifact id both arrive from outside and both are joined under the
  state root; `ids::is_safe_name` is where that is rejected.
- **A provenance trailer is written and read under one prefix, and the crate knows
  no particular value of it.** Every reader takes the `provenance::Trailers` the
  writer used — an asymmetry here is silent, and its cost is an incomplete branch
  published as complete. That is also why a marker under an *unconfigured* prefix
  is refused rather than ignored: `provenance::unrecognized` matches the marker's
  own shape, never a particular consumer's spelling, and `recoverable`,
  `integrate`, and `recover` each name what they found. Special-casing a prefix
  value would be this crate learning a consumer's vocabulary, which it must not.
