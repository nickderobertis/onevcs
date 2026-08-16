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

## Three verbs land a branch, and provenance is what chooses between them

`publish` takes a session token; `recover` and `publish-branch` take a branch name
and are **one path** (`branch.rs`), because a second locate, clone, or base-merge
beside it is drift nothing would catch. What separates them is provenance and
nothing else: `recover` requires an unattested incomplete marker and writes the
attestation that clears it, `publish-branch` requires that there is none.
`integrate` stays the local-only merge train and routes to `publish-branch` rather
than re-gating a branch itself.

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

`recoverable` is the report `recover` and `publish-branch` are reached from, so
the command it prints per row is one of them, by path (`--repo`) rather than by
cwd. The train is deliberately not what it names, even for finished work:
`integrate` reads its candidates out of the publication checkout alone and
refuses a team or remote identity outright, so it lands none of the branches this
report is most often read about — the ones a run left in its own clone.

## Two verbs that read and plumb, so nothing has to reach for raw `git` or `gh`

`status` and `import` exist because an agent that needed either answered it outside
this boundary. Three things about them are easy to undo by accident.

- **`status` reads landing off content, never off ancestry or off the host.**
  Publication squashes, so a branch that landed is an ancestor of nothing
  afterwards — the base simply carries what it changed, which is the same question
  `vcs::collect` excludes a branch on. Reading the absence of an open change
  request instead is what reported an already-merged change as unpublished, and it
  is the defect the verb exists to close. `recoverable` was never blind to any of
  this; what was missing is that its *exclusion reason* — landed, still held by an
  open session, or genuinely preserved — was legible nowhere.
- **A change request's URL is resolvable only through the event stream**, because
  nothing on a branch carries it. That is the one spelling `status` cannot answer
  for a change something else opened, and it is recorded as an open question in
  `docs/inferred-surface.md` rather than closed by inventing a trailer or a trait
  method.
- **`import` writes refs and nothing else.** It fetches into a scratch ref, judges
  there, and points the destination's ref at the result — so no working tree in any
  registered checkout moves, and a name the destination has *checked out* is refused
  rather than written. It refuses a non-fast-forward by naming the commits that
  would go, because the way through is `--as`, and that is only the right way
  through once an operator can see what the name they asked for already holds.

Where it looks for a branch nobody named is `branch::locate`, which is where the
two publishing verbs look. One search over the places an identity keeps work — run
clones included — so a branch a verb can land is a branch this can reach.

## What a report answers about, and what a name already means

Preserved work goes missing through silence rather than through a search that
could not reach it, so two things are stated rather than left to be inferred.

- **A scoped answer names its scope.** `recoverable` answers for one identity when
  it is run inside a registered checkout and for every identity when it is not, and
  nobody types which — the directory decides. An answer that does not say so reads
  as the whole host's, and another identity's preserved work then reads as work
  nobody has. Every rendering names the scope, `--json` included (on stderr, where
  a parser does not meet it).
- **A branch pin is honoured or refused, never quietly cut fresh.** A session's
  branch is cut from the base with `worktree add -b`, so a pin naming a branch that
  already carries work produces a second, empty branch of that name: the session
  reports the pinned name, carries none of the commits, and cannot hand the name
  back either, since a branch is only ever copied out fast-forward. `workspace::open`
  asks every repository the identity keeps branches in — the run clones included —
  and origin's own copy, and refuses unless the base already carries what the name
  means. The bar is not that the name is unused; it is that the session carries
  whatever the name refers to.

## Tests are journeys, and there are no unit tests

This crate carries no `#[cfg(test)]` module. `tests/contract.rs` holds the
approved surface to the contract text it is extracted from; everything else in
`tests/e2e/` spawns the compiled binary and drives it against real git. A path
only an in-process test could reach is a path to delete, not one to unit-test —
which is also how the 95% coverage floor is met.

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

`tests/e2e/world.rs` is the fixture, and it is Unix-only: the program it installs
as `gh` and the `pre-push` hooks the gate journeys write are POSIX shell, and a
fired timeout takes a process *group*, which has no portable spelling. Windows CI
builds the crate and runs the contract, boundary, and packaging suites.

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
- **The scratch repository declares no required check.** `gate: {kind: checks}`
  waits for checks that *block*, so that path cannot go green there and stays
  covered by the offline tier; this tier proves `change_checks` and `check_log`
  themselves against the real workflow the repository carries. Making its check
  required would need branch protection, which would then also gate every merge
  this tier depends on.
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
