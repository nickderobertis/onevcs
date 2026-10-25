//! The end-to-end journeys.
//!
//! Every one of these spawns the real artifact as a subprocess — the compiled
//! `onevcs` binary, or the committed script a workflow runs — and asserts on its
//! exit code, stdout, and stderr, the way a user or a CI job meets it. Nothing
//! here calls into the library.
//!
//! Eight modules are the exception, and they are the ones *about* the library:
//! `honesty`, `seam`, `library` and `drafts` drive it in-process because supplying an
//! implementation is something only a caller embedding the crate can do — the
//! binary deliberately has no flag for it — `publication_watch` because handing a
//! publication a cancellation is the same kind of thing, `operation_progress`
//! because a prepared invocation and its reader are library calls no verb renders,
//! and `declaration` and
//! `discovery` do
//! because what they hold *is* the promise that a linking consumer reaches these
//! answers without spawning anything, which no binary can show. Each says so at its
//! head.

// Unix only: `accounting` publishes through the same substituted `gh` as `host.rs`,
// and cuts real sessions and real run clones. Its own header carries the reason in
// full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod accounting;
// Unix only: publications through the same substituted `gh` as `host.rs`, which holds
// an armed change and lands it once its checks pass; one journey drives the library in
// process. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod armed;
// Unix only: continuations and publications through the same substituted `gh` as
// `host.rs`, whose branch protection decides whether a moved base is merged. Its own
// header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod base_merge;
// Unix only: `change` opens drafts through the same substituted `gh` as `host.rs`
// and cuts real sessions. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod change;
// Unix only: the prefix a host configures and the name a caller proposes, driven
// against real branches in real clones. Mostly the compiled binary; three journeys
// drive the library seam in process, for the reason `library.rs` gives. Its own
// header carries the rest.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod branches;
// Concurrent CLI publications configure the same real git lender.
#[cfg(unix)]
mod borrower_config;
// Unix only: the public boundary, through `boundary check`, `inspect` and `schema`,
// over real repositories and the substituted `gh` that answers a repository's
// visibility. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod boundary;
mod cli;
// Unix only: one journey makes the state root unwritable by mode, to hold that a
// write which cannot complete installs nothing.
#[cfg(unix)]
mod rules_apply;
// Unix only: `onevcs export` from a real private clone into a real public checkout,
// with visibility answered by the substituted `gh`. Its own header carries the reason.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod export;
// Unix only: every publication entry point held to the public boundary, through the
// substituted `gh` and real bare origins. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod publication_boundary;
// Unix only: the host's public phrases, through `boundary check` and publications over
// the substituted `gh` and real bare origins. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked,expensive_tests_stay_behind_their_own_edge] the host
// boundary is substituted as explained above; these journeys join the existing
// onevcs-e2e edge beside `boundary`, `export` and `publication_boundary`, whose subject
// they share, and what reruns that tier is its `e2eInputs` in `nx.json`, not this module.
mod public_phrases;
// The producer's own release declaration. It drives the binary and, for the half of
// the promise a binary cannot show, the library beside it — see its own header.
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod declaration;
// Unix only: it commits declarations and probe scripts into a real checkout and lands
// real publications, all through `world.rs`'s POSIX fixture. It drives the binary and,
// for the promise a binary cannot show, the library beside it — see its own header.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod discovery;
// The terms two backends' event streams are compared on. Shared with the
// real-backend tier in `tests/smoke`, so one leg cannot accept a difference the
// other rejects.
mod comparison;
// Every platform: concurrent sessions of one repository over real git, with a fixture
// of its own because `world.rs` is Unix-only. Its own header carries the reason.
mod concurrent_sessions;
// Unix only: it installs a counting `git` on `PATH` as a POSIX shell script, and cuts
// real sessions and real publications through `world.rs`'s fixture. Its own header
// carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod cost;
// Unix only, like `cost`, whose counting `git` it installs: the shim is a POSIX shell
// script.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] the counting `git` execs the real one; see the module's header.
mod git_starts;
// Linux only: coverage is measured on Linux alone, so `cargo llvm-cov` is installed
// there and nowhere else; it drives `scripts/coverage.sh` over a scratch crate.
#[cfg(target_os = "linux")]
mod coverage;
// Unix only: the draft lifecycle, row by row, through real git against a real bare
// origin and the host from `onevcs-testing` — a supplied host is the seam the rows are
// driven through, and the crate a consumer drives them with. Its `rules check` journeys
// and one publication through the real `GitHub` implementation drive the binary. Its
// own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod drafts;
// Unix only: it reads the call log `world.rs`'s substituted `gh` writes.
#[cfg(unix)]
mod call_baseline;
#[cfg(unix)]
mod edges;
// Linux only: it lets a held fetch go once the kernel's lock table shows the second
// fetch queued behind it, and `/proc/locks` is the only place an outside process can
// read that. Its own header carries the reason in full.
#[cfg(target_os = "linux")]
mod fetch_turns;
// Linux only: the stamping `git` and the hooks it is measured against read the clock
// with GNU `date +%s%3N`. The required-checks journeys are in-process for the reason
// `drafts.rs` is, and its own header carries the rest.
#[cfg(target_os = "linux")]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod gate_runs;
// Unix only: `filter` publishes through the same substituted `gh` as `host.rs`. Its
// own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod filter;
// Unix only: it copies this checkout, shares its Nx install through a symlink, and
// drives the real Nx over it, through POSIX scripts. Its own header carries the reason the caching
// journey counts runs of two recipes it replaces.
#[cfg(unix)]
mod graph;
// Unix only: these drive a substituted `gh` and real `pre-push` hooks, both POSIX
// shell. See `world.rs`.
// `honesty` compares the real backend against the test one, so one of its two runs
// is a test backend by construction — the subject, not a shortcut around it. Its own
// header carries the reason in full.
#[cfg(unix)]
mod holders;
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod honesty;
// Unix only: its hooks are POSIX shell, and its change-policy journey publishes
// through the same substituted `gh` as `host.rs`. Its own header carries the reason.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod hook_environment;
#[cfg(unix)]
mod host;
// Unix only: the program that replays GitHub's recorded responses as `gh` is POSIX
// shell. Its own header carries the reason it is the one boundary doubled.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod host_queries;
// Linux and Windows: it takes a duplicate of another process's pipe, which is
// `/proc/<pid>/fd/1` on one and `DuplicateHandle` on the other, and macOS offers an
// unrelated process neither of those. Its own header carries the reason in full.
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod inherited_pipes;
// Unix only: its hosted journeys publish through the same substituted `gh` as
// `host.rs`, and two of them land a branch on a base without this crate at all. Its
// own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod landing;
// `library` drives the typed library surface on both backends: half of what it
// compares is a supplied implementation by construction, and the other half is the
// real `Git` and the substituted `gh`. Its own header carries the reason in full.
#[cfg(unix)]
mod labels;
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod library;
#[cfg(unix)]
mod lifecycle;
// The additive operation API is a library interface; these journeys use real git
// and only the existing third-party hosting provider boundary. They sit in this
// project because what they test is this crate's own publication, recovery,
// integration and stream code — exactly the edge onevcs-e2e has — and they reach no
// live service: each runs in 0.4–2.6s against local git, under the `gate_runs` and
// `publication_watch` journeys beside them.
#[cfg(unix)]
// llmlint: ignore[expensive_tests_stay_behind_their_own_edge] see the note above this module's declaration.
mod operation_progress;
// Linux only: it counts a directory's own `opendir` calls through `inotify`, which
// only Linux has. Its head carries the reason the counting is outside the process,
// and the journeys that use it are gated the same way.
#[cfg(target_os = "linux")]
mod listings;
#[cfg(target_os = "linux")]
mod lookups;
// Unix only: its maintenance command is a POSIX shell script, and a fired bound takes a
// process *group*, which has no portable spelling. Its own header carries the reason.
#[cfg(unix)]
mod maintain;
// Unix only: a continued branch whose base conflicts with it, opened over the merge and
// then published, preserved, adopted, closed, swept and retired through the binary, on
// `world.rs`'s POSIX fixture and in real slots. Its own header carries the reason.
#[cfg(unix)]
mod open_conflict;
mod packaging;
// Unix only: it runs this repository's bash measurement script over a one-crate slot.
// Its own header carries the reason.
#[cfg(unix)]
mod pool_maintenance;
// Unix only: real slots under a real state root, cut and returned through the binary,
// with the typed surface's in-process journeys at its end; its own header carries the
// reason.
#[cfg(unix)]
mod pool;
// Unix only: it drives real bare origins, real clones, real `git push`es and real
// `pre-push` hooks, and one of its journeys uses the same substituted `gh` as `host.rs`
// to assert that a preserving push reaches no host at all. Its own header carries the
// reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod preserve;
// Unix only: it drives real bare origins, real clones, real merges and real `git
// push`es, real `pre-push` hooks, and the same substituted `gh` as `host.rs` to assert
// that a stack base opens no change request. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod stack_base;
// Unix only: its hosted journeys publish through the same substituted `gh` as
// `host.rs`. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod publish_branch;
// Unix only: a publication's watch ending on a conflict the host confirms and on its
// caller's cancellation, through the same substituted `gh` as `host.rs` and real git.
// In-process, because a cancellation is handed over only by a caller embedding the
// crate. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore-block[e2e_not_mocked] see the note above this module's declaration: a
// cancellation is handed over only in-process, and the one stand-in is the substituted
// `gh` every hosted journey here uses, over real git and a real bare origin.
mod publication_watch;
// llmlint: ignore-end[e2e_not_mocked]
// Linux only: it mounts a filesystem of its own, and an unprivileged mount there
// needs nothing outside the distribution's own `fuse3`. Its head carries the reason
// in full, and the one journey that uses it is gated the same way.
#[cfg(target_os = "linux")]
mod refusing_fs;
#[cfg(unix)]
mod registry;
// Unix only: a required context the host declares and has not started, published
// through the same substituted `gh` as `host.rs` over real git and a real bare origin.
// Its own header carries the reason in full.
// Linux only, like `cost::Counting::inhabit`, which puts the counting `git` on this
// process's own `PATH`: the reads are the library's, so the count is this process's.
// The one program substituted is `git`, with itself; its own header says so in full.
#[cfg(target_os = "linux")]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod repeated_reads;
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod required_contexts;
// Unix only: its journeys publish through the same substituted `gh` as `host.rs`, over
// real git and a real bare origin. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod resumption;
// Unix only: a landing releasing what it built in, driven through real publications
// whose POSIX-shell gates start real processes inside the workspace. One ending goes
// through the same substituted `gh` as `host.rs`; its own header carries the reason.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod released;
// Unix only: a git hook the step runs is what opens the retry inside it, and its hooks
// are POSIX shell. Everything is local: real bare origin, real clones and slots, the
// compiled binary on both sides of the race.
#[cfg(unix)]
mod record_writes;
// Unix only: its journeys publish through the same substituted `gh` as `host.rs`.
// Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod retries;
// Unix only: its journeys cut real sessions on a real pool slot through `world.rs`'s
// POSIX fixture, and the ones about a change request publish through the same
// substituted `gh` as `host.rs`. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod retire;
// Unix only: several repositories publishing one branch name through the same
// substituted `gh` as `host.rs`, one repository of it each, over real origins and real
// sessions. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked,expensive_tests_stay_behind_their_own_edge] the host
// boundary is substituted as explained above; these ordinary one/two-repository
// journeys use the existing onevcs-e2e edge. The expensive 19-identity hour workload
// lives behind its own onevcs-recovery edge.
mod own_change;
// Unix only: its probes are real POSIX shell scripts and real `sh -c` one-liners,
// and its landings are real local-direct publications. Its own header carries the
// reason in full.
#[cfg(unix)]
mod releases;
// Unix only: its journeys read and answer review feedback through the same substituted
// `gh` as `host.rs`, whose review routes `review_host.rs` serves from the journey's own
// process. Each module's own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod review;
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod review_host;
// Unix only: where a stack of drafts stands, and the moves that keep it true to the
// host, driven through the same substituted `gh` and `review_host.rs` as `review`, with
// a person's merges done in real git against the real bare origin. Its own header
// carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod standings;
// `seam` proves each command reaches the implementation it was *handed*, which cannot
// be shown without handing it one. Everything else in it is real: real bare origins,
// real clones, a real `git push`, real session records.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod seam;
// Unix only: the scripts these drive are POSIX shell. See each module's own note.
#[cfg(unix)]
mod scripts;
#[cfg(unix)]
mod smoke;
// The guard that keeps every other module here off the operator's own state root.
// It reads this repository's sources rather than driving the binary, which is the
// only way to see a spawn that does *not* exist — the thing that went wrong was a
// second one nobody remembered.
mod state_root;
// Unix only: the `commit-msg` hooks these install are POSIX shell.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod subject_policy;
mod support;
// Unix only: it drives real publications and a real `pre-push` gate, both POSIX
// shell, and backdates run roots with Unix file times. Its publications go through
// the same substituted `gh` as `host.rs`; its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod sweep;
// Unix only: it installs `cost.rs`'s counting `git` and `world.rs`'s `gh`, both POSIX
// shell, over real origins and real sessions. Its own header carries the reason in full.
#[cfg(unix)]
// llmlint: ignore[e2e_not_mocked] see the note above this module's declaration.
mod verdicts;
#[cfg(unix)]
mod world;

// Linux loader interposition observes the real lock open without timing.
#[cfg(target_os = "linux")]
mod checkout_reads;
