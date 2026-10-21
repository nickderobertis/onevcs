# Concurrent session Git-file acceptance evidence

Implementation tested: `96ac537` (including the portable Windows fixture repair).
Evidence collected by this dispatch on Linux with Git 2.43.0, through the compiled
`onevcs` CLI and real Git repositories. No CI was triggered, rerun, or cancelled.
The report commit changes documentation only.

## Files and operations

The registered checkout is shared by all sessions. Closed run clones and reused
pool-slot clones are also read by sibling branch searches. A session's linked
worktree has its own HEAD and index; those are not the shared checkout's HEAD
and index.

| Operation | Shared Git files affected | Finished behavior and reason |
| --- | --- | --- |
| Open: retain the registered checkout's borrowed objects | `.git/config` | Already-correct `gc.auto=0` and `gc.pruneExpire=never` cause no write. Registration installs these for new registrations. An earlier release's registration needs one real change: append the missing settings in place while holding Git's `config.lock` and the exclusive lender lease. No rename or truncation makes the file unopenable. Every session read, including `is_repo`, follows acquisition of the lender lease. Outside Git readers need no onevcs lease: the append exposes only parseable intermediate states. |
| Open: construct or reconstruct a run/slot clone | Clone `config`, `HEAD`, `packed-refs`, `objects/info/alternates`, remote refs including `refs/remotes/origin/HEAD` | Initial clone/configuration writes occur under `.clone.cutting`, before moving the complete clone into its published path. A URL is set only if different. A sibling never sees the partially configured clone. Reclamation is excluded while a sibling holds the clone's read lease. |
| Open: reuse a slot and cut its worktree branch | Slot `config`; `refs/remotes/origin/HEAD`; local and remote branch refs; linked-worktree administrative files | Branch creation uses `--no-track`, so it adds no upstream settings and rewrites no shared config. `origin/HEAD` is read first and is not rewritten when already symbolic to the requested base. A different base requires the symbolic ref to change. Changed branch refs transfer the lender's current origin view and establish the selected branch/base. |
| Open: advance a reused clone’s local base | Clone `HEAD` and `refs/heads/<base>` | When the local base can fast-forward, `carry_base` first detaches the clone’s HEAD if it still names that base, then advances the branch. Otherwise its HEAD/index would describe a branch moved behind its back. The trace captured seven published-clone HEAD replacements by `update-ref --no-deref HEAD`. These change symbolic HEAD into a direct object ID; subsequent aligned reuse does not repeat them. HEAD is a ref, with the same Windows delete-sharing reader path. |
| Fetch (also performed by opens/restacks) | `refs/remotes/origin/*`, potentially `packed-refs` when pruning; `FETCH_HEAD`; newly received object files; Git bookkeeping such as ref logs | A repository-owned fetch lock serializes competing fetch writers. Ref values change when origin changes, or disappear when pruned. Their replacement is required to represent that new state. Git's ref readers permit deletion sharing on Windows. `FETCH_HEAD` is fetch bookkeeping written in place, not replaced through a config-style rename; sibling session operations do not consume it. Received object names identify immutable content. |
| Restack/continue | Clone local branch refs, `refs/onevcs/continued`, remote refs; linked-worktree `HEAD`, `ORIG_HEAD`, index and merge state | Opens/fetches have the behavior above. The caller's real merge/commit changes its own worktree; continuation fetches the chosen branch into a scratch ref. These are required branch/state changes, not shared-config rewrites. Sibling searches read refs/objects, not another session's worktree index/merge state. |
| Close | Registered checkout `refs/heads/<branch>`; clone branch refs when synchronizing a worktree; linked-worktree administrative entries removed by `worktree remove` | Required to hand back committed work. The checkout branch tip changes to the closed session's tip. Ref readers use Windows delete sharing. Worktree removal concerns the closing session's worktree, and close refuses a live occupant. Shared clone reads run from outside it and hold reclamation leases. Close does not rewrite shared config or unchanged `origin/HEAD`. |

Locks (`onevcs-retention.lock`, the fetch lock, `config.lock`, ref lock files) are
coordination files, not repository inputs read by sibling Git commands. New object
and ref-log writes do not replace the existing configuration that Git opens.

The Windows distinction is grounded in the source of the Git version reported by
the failed job, 2.56.0.windows.2: [mingw.c](https://github.com/git-for-windows/git/blob/v2.56.0.windows.2/compat/mingw.c)
uses delete sharing for existing-file `open` reads, while `fopen` follows the CRT
path; [files-backend.c](https://github.com/git-for-windows/git/blob/v2.56.0.windows.2/refs/files-backend.c)
reads loose refs through `open`. Thus a moving ref can be replaced while readers
retain their handles; configuration replacement has different sharing behavior.
This is source evidence, not a claim that this Linux dispatch ran Windows tests.

## This dispatch's traces

Commands, run from the implementation checkout:

```sh
strace -f -qq -s 300 -e trace=execve,rename,renameat,renameat2,openat \
  -o /tmp/onevcs-session-repair.strace \
  just test-one concurrent_session_operations_in_one_repository_never_fail_a_read_beside_them
strace -f -qq -s 180 \
  -e trace=execve,openat,rename,renameat,renameat2,unlink,unlinkat,write,pwrite64 \
  -o /tmp/onevcs-acceptance-final.strace \
  just test-one concurrent_sessions > /tmp/onevcs-acceptance-final.log 2>&1
```

Results: the first ran one test, passed in 16.780s; the second ran all 11
concurrency journeys, all passed in 59.300s. Both commands exited 0.
The latter covers first opens, fetches, restacks, closes, reused slots, upgrade
read overlap, failed configuration-lock acquisition, and configuration end shapes.

Trace observations (fixture setup is distinguished from session operations):

- In the first trace, after the first session-open exec, there were zero
  registered-checkout `config` renames. Thirteen registered-checkout ref renames
  established/advanced `placement/{a,b,c}`, `sibling/{a,b,c}` and
  `refs/remotes/origin/{main,placement/a,placement/b,placement/c}`.
- Across the complete final trace, there were zero replacements of `config`,
  `packed-refs`, or `refs/remotes/origin/HEAD` at published run/slot clone paths.
  Their clone-construction replacements were under `.clone.cutting`.
- The upgrade writes opened the existing checkout `config` with `O_RDWR`, without
  `O_TRUNC`. For example PID 3214511 opened `.tmphicYMu/project/.git/config` as fd 5
  and used `pwrite64` to add the comment containing the retention settings. This
  is an in-place write, followed by the single-byte line-ending changes.
- The original-tree trace below captured repeated registered-checkout
  `config.lock -> config` renames and pool-clone config replacements. PID 3288033
  ran `git worktree add -b placement/a .../pool/1/worktree origin/main` without
  `--no-track`; PID 3305636 renamed the shared checkout config during the concurrent
  journey. The unchanged-file assertion independently caught the replacement.

## Before-tree commands and results

The final regression and support files were overlaid onto historical production
trees. This lets the same real CLI journey challenge old implementations; production
code was not patched to manufacture a failure. A temporary test entry point imports
those two files, so unrelated historical e2e modules need not be rebuilt. All builds
used `/tmp/onevcs-acceptance-target`; the temporary worktree was
`/tmp/onevcs-acceptance-before`.

Preparation from the implementation checkout:

```sh
git worktree add --detach /tmp/onevcs-acceptance-before 1bf557c
cp crates/onevcs/tests/e2e/concurrent_sessions.rs /tmp/concurrent_sessions.acceptance.rs
cp crates/onevcs/tests/e2e/support.rs /tmp/support.acceptance.rs
```

For each revision/selection in the table, run these commands from that temporary
worktree (the reset affects only the disposable worktree):

```sh
git reset --hard "$revision"
cp /tmp/concurrent_sessions.acceptance.rs crates/onevcs/tests/e2e/concurrent_sessions.rs
cp /tmp/support.acceptance.rs crates/onevcs/tests/e2e/support.rs
cat > crates/onevcs/tests/acceptance.rs <<'RS'
#[path = "e2e/support.rs"]
mod support;
#[path = "e2e/concurrent_sessions.rs"]
mod concurrent_sessions;
RS
CARGO_TARGET_DIR=/tmp/onevcs-acceptance-target cargo nextest run --locked \
  -p onevcs --test acceptance -E "$selection" --no-fail-fast --status-level fail
```

The traced original-tree replay prefixed that cargo command with:

```sh
strace -f -qq -s 180 -e trace=execve,openat,rename,renameat,renameat2 \
  -o /tmp/onevcs-acceptance-original.strace
```

| Production revision | Exact selection | Result |
| --- | --- | --- |
| `1bf557c` | `test(concurrent_session_operations_in_one_repository_never_fail_a_read_beside_them)` | Initial untraced run: exit 100, 1 failed in 0.979s. Shared config identity assertion failed; modification time changed from `1792610606.337471920` to `1792610606.847614985`, even though the inode number was reused. |
| `1bf557c` | `test(concurrent_session_operations_in_one_repository_never_fail_a_read_beside_them) \| test(concurrent_sessions_in_reused_pool_slots_never_fail_a_read_beside_them)` | Traced replay: exit 100, both failed in 16.545s. Run-root continuation encountered a reclaimed clone; pool journey failed the shared config identity assertion, including modification time. |
| `02ea8a7^` (`6cd1315`) | `test(a_checkout_an_earlier_release_registered_is_given_its_retention_keys_while_git_reads_it)` | Exit 100, 1 failed in 1.566s: an open ended while the retention write's lease was held and config was unopenable, reporting that the project was not a Git checkout. This catches the pre-lease `is_repo` read. |
| `40c9a28` | `test(concurrent_sessions_in_reused_pool_slots_never_fail_a_read_beside_them)` | Exit 100, 1 failed in 2.387s at unchanged `origin/HEAD` identity equality. Modification time changed from `1792610711.132639331` to `1792610712.946541310`; inode/birth also changed. |
| `1654c43^` (`0ab31aa`) | `test(an_upgraded_checkout_whose_last_line_continues_into_the_end_of_the_file_opens_beside_its_readers)` | Exit 100, 1 failed in 0.409s: the prior setting `onevcs-journey.note` changed from `kept ` to `kept [gc] auto = 0`. The file parsed, but the existing value was corrupted. |

Logs are `/tmp/onevcs-before-original.log` (initial untraced run) and
`/tmp/onevcs-replay-{original,lease,origin-head,unterminated}.log`.
The replay driver used the commands above and stopped unless each historical
run returned nextest's test-failure exit 100. Compilation failures would not have
been accepted as regression evidence. The temporary test harness imported the
final regression without changing any assertion.

## What the passing final regression asserts

`concurrent_sessions.rs` starts three continued/restacking nodes and three fresh
sibling opens/closes alongside two repeated Git reader loops. The base advances on
origin, and the test requires every observed branch tip to have changed, so fetch
and close writes really occur while the readers run. Reads include branch lookup,
listing, configuration and diff. Every operation must succeed, and reader stderr
is checked, including warnings that might accompany an otherwise successful exit.

For the registered checkout `config`, every reused-slot `config`, and each slot's
`refs/remotes/origin/HEAD`, the test compares the complete `Identity` before/after:
Unix device/inode, birth time, **and modification time**; Windows creation time
and **modification time**. These are full equality comparisons, not the helper
that deliberately permits a changed modification time for an in-place upgrade.
The run-root config is held from registration through first opens and subsequent
restacks/closes; slot files are held from warm-slot creation through reuse.

The upgrade regression removes both retention keys to reproduce an earlier
release's registration. It holds the lender lease and makes config unopenable,
starts the first three opens inside that window, and requires them to wait. It
then restores config while retaining the lease, starts outside Git readers,
requires completed read rounds **before releasing the lease**, and lets the first
retention write happen with those readers already running. Reads continue through
all first opens and subsequent restacks/closes. The existing config file must
retain its identity, and its non-retention settings must retain their values.
Its modification time may change because this upgrade genuinely adds settings.

The same final test group covers a config ending in a continued line, a last line
ending in a backslash without a newline, an ordinary last line without a newline,
and an empty local config whose origin comes from an include. It compares Git's
parsed non-retention settings before and after the write. The Windows fixture
repair uses forward slashes for the included origin URL and asserts Git reads the
complete URL before opening sessions.

## Final implementation checks and CI routing

On `96ac537`, the dispatch ran:

| Command | Result |
| --- | --- |
| `just test-one concurrent_sessions` | Exit 0; 11 passed, 13.350s. Repeated under the trace above: 11 passed, 59.300s. |
| `just test-quick` | Exit 0; Nx reported every requested test target succeeded, including the existing session journeys and recovery workloads. |
| `just fmt-check` | Exit 0 after applying the formatter's requested layout. |
| `just lint` | Exit 0; all requested deterministic lint targets succeeded. |
| `just lint-llm-diff` | Exit 0 on the formatted committed implementation; 20 rules passed, 0 failed, 17 skipped, 2 not relevant. History `20261021T185901Z-ac2ca`. |

The historical Windows failure was inspected read-only with
`gh api repos/nickderobertis/onevcs/actions/jobs/119408641466/logs`.
It failed the empty-local-config regression with `bad config line 2` in the
included origin file. A real `git config --file` reproduction with a Windows
backslash path returned 128; replacing separators with `/` returned 0 and the
complete URL. No finished-tree Windows run is claimed here.

CI already includes these journeys without OS exclusions:

- `.github/workflows/ci.yml`: Linux `gate` runs `just check-affected`; Windows
  `cross` and the existing macOS `macos-pr` lane each run `just check-quick`.
  The macOS lane retains its existing policy condition; this change does not
  broaden or narrow when that lane is selected.
- `crates/onevcs/tests/e2e/main.rs` imports `concurrent_sessions` unconditionally.
  `justfile` includes it in `e2e-tier`, and the `onevcs-e2e` Nx project runs that
  tier in both its instrumented `test` and uninstrumented `test-quick` targets.
- `nx.json` includes crate sources and e2e inputs in the relevant affected/cache
  inputs, so a session implementation or regression change reaches that tier.

The report adds no behavior or enforcement changes. Documentation-only final
checks are recorded in the dispatch response. Temporary historical worktrees and
build output are removed after replay; raw trace/log evidence remains under `/tmp`.
