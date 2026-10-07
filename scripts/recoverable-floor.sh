#!/usr/bin/env bash
# The recoverable-floor harness: how fast "does this session owe a preserved branch?" is.
#
# Spike `spike-recoverable-floor` keeps this on its preserved branch for the nodes that
# implement the fast read, so one of them can register it as, or grow it into, a budget
# command. Two verbs:
#
#   scripts/recoverable-floor.sh fixture [--scale N] [--dir DIR] [--identities N]
#       [--labelled N] [--kept N] [--history N]
#       Build — or reuse, when DIR already holds one built to that shape — a scratch
#       ONEVCS_HOME shaped like the host this was measured on: 20*N identities with real
#       bare origins and registered checkouts, ~19 kept session records per identity of
#       which ~4 on half of the identities carry the measured launcher label, ~4.3 event
#       streams per kept record (a swept history, as `onevcs sweep` leaves one), and the
#       measured sessions' branches spread over every recovery state: landed, retirable,
#       superseded-with-changes, held by a live session, unlanded (`no`), `unknown` and
#       `in-part`. Every record, branch and stream is made by the real `onevcs` verbs and
#       real git; nothing is written by hand. The shape options override the scale's
#       identity count, labelled identities, kept records per identity and swept
#       sessions per identity, for a fixture smaller than the host's.
#
#   scripts/recoverable-floor.sh run READ [--scale N | --dir DIR | --real] [--runs N] [--cold]
#       [--session ID] [--load K] [--count-runs M] [--dispatches]
#       Run one named read N times and print one line: the wall-time distribution
#       (median, p90, max, in ms), the git process count from M separate runs under a
#       counting `git` shim (never the timed runs), load1 at the start and end, and the
#       commit this tree is at. READ is one of:
#         v0.42.0       `onevcs recoverable --json --label launcher=ID` from `/`, by the
#                       baseline binary (--baseline, default the `onevcs` on PATH)
#         legacy        the same read by this branch's binary with the prototype off
#         prototype     this branch's prototype, every field v0.42.0 answers
#         decision      this branch's prototype, only what a verdict reads, plus `tip`
#         stop-verdict  `scripts/unpublished.sh --stop-verdict` of the ai-orchestrator
#                       checkout (--aio), fed {"session": ID, "continuation": false}
#         stop-guard    `onepipeline stop-guard --session ID --format neutral`
#         onevcs-version, onepipeline-version   process start-up alone
#       --cold empties the prototype's proof cache before every run (OS caches are not
#       dropped: that needs root); without it one unmeasured run primes the cache first.
#       --real reads this host's own ~/.onevcs and writes nothing there: the prototype's
#       cache goes under the harness's state directory instead. --load K runs K
#       generated dispatch-like workers (gzip and `git log -p` loops) for the whole set,
#       after a 60 s warm-up so load1 reflects them, and stops them after.
#       --dispatches counts the live dispatches `onepipeline host` lists (slow: ~30 s).
#       --profile prints a second line, `profile=` and the in-process profile of the
#       last timed run (this branch's reads only): phase times, git time, cache hits.
#
# Environment: RECOVERABLE_FLOOR_DIR selects fixture/state storage; ONEVCS_BIN
# selects the prototype artifact. --help prints both resolved paths. For a
# development artifact, use `just run --version` and set ONEVCS_BIN to its binary.
set -euo pipefail

repo_root=$(CDPATH='' cd -- "$(dirname -- "$0")/.." && pwd)
state=${RECOVERABLE_FLOOR_DIR:-${ONEPIPELINE_NODE_SCRATCH_DIR:-$repo_root/target}/recoverable-floor}
onevcs_bin=${ONEVCS_BIN:-$repo_root/target/release/onevcs}
real_git=$(command -v git) || {
    echo "recoverable-floor: no git on PATH; install git, then retry" >&2
    exit 2
}

die() {
    echo "recoverable-floor: $1" >&2
    exit "${2:-2}"
}

usage() {
    sed -n '2,46p' "$0" | sed 's/^# \{0,1\}//'
    printf '\nResolved paths: RECOVERABLE_FLOOR_DIR=%s ONEVCS_BIN=%s\n' "$state" "$onevcs_bin"
}

# The measured launcher, and the other managers whose sessions fill the host around it.
measured_launcher=fixture-launcher-measured

fixture_dir() { echo "$state/scale-$1"; }

# One `onevcs` call against the fixture's state root.
fx() {
    HOME="$fixture" ONEVCS_HOME="$fixture/home" ONEVCS_LOCK_TIMEOUT_SECONDS=600 \
        "$onevcs_bin" "$@"
}

fx_git() {
    HOME="$fixture" "$real_git" "$@"
}

# Open a session and print "TOKEN WORKTREE".
open_session() {
    local checkout=$1 branch=$2 launcher=$3 node=$4 opened token worktree
    opened=$(fx session open "$checkout" --branch "$branch" --label "launcher=$launcher" \
        --label "run=fixture-run" --label "node=$node") ||
        die "session open $branch in $checkout failed" 1
    token=$(sed -n 's/.*"token":"\([^"]*\)".*/\1/p' <<<"$opened")
    worktree=$(sed -n 's/.*"worktree":"\([^"]*\)".*/\1/p' <<<"$opened")
    [ -n "$token" ] && [ -d "$worktree" ] || die "session open printed no token: $opened" 1
    echo "$token $worktree"
}

commit_in() {
    local worktree=$1 file=$2 contents=$3 subject=$4
    mkdir -p "$(dirname "$worktree/$file")"
    printf '%s\n' "$contents" >"$worktree/$file"
    fx_git -C "$worktree" add -A
    fx_git -C "$worktree" commit -q -m "$subject"
}

close_session() {
    fx session close "$1" >/dev/null || die "session close $1 failed" 1
}

land() {
    fx publish-branch "$2" --repo "$1" >/dev/null || die "publish-branch $2 failed" 1
}

# open, commit one file, close: a session that left its work preserved.
worked() {
    local checkout=$1 branch=$2 launcher=$3 node=$4 file=$5 contents=$6 subject=$7 opened
    opened=$(open_session "$checkout" "$branch" "$launcher" "$node")
    commit_in "${opened#* }" "$file" "$contents" "$subject"
    close_session "${opened%% *}"
}

expect() {
    printf '%s\t%s\t%s\n' "$1" "$2" "$3" >>"$fixture/expected.$4.tsv"
}

# One recovery state, made the way a host comes to hold it, for the launcher given.
scenario() {
    local checkout=$1 k=$2 n=$3 state_name=$4 launcher=$5 b opened
    b="work/r$k-$n-$state_name"
    case "$state_name" in
        landed)
            worked "$checkout" "$b" "$launcher" "n$n" "src/$b.txt" "$b" "feat: $b"
            land "$checkout" "$b"
            ;;
        retirable)
            # A second attempt with the same content landed: this branch holds nothing
            # beyond its base, which content alone proves.
            worked "$checkout" "$b" "$launcher" "n$n" "src/$b.txt" "same" "feat: $b"
            worked "$checkout" "$b-twin" "$launcher" "n$n-twin" "src/$b.txt" "same" "feat: $b again"
            land "$checkout" "$b-twin"
            ;;
        superseded)
            worked "$checkout" "$b" "$launcher" "n$n" "src/$b.txt" "first" "feat: $b"
            worked "$checkout" "$b-retry" "$launcher" "n$n-retry" "src/$b.txt" "second" "feat: $b retried"
            land "$checkout" "$b-retry"
            fx supersede "$b" --repo "$checkout" --by "$b-retry" \
                --landing "$(fx_git -C "$checkout" rev-parse origin/main)" \
                --label "node=n$n" >/dev/null || die "supersede $b failed" 1
            ;;
        live)
            # Left open: the harness holds its run root's lease while it measures, which
            # is what makes the session's hold a live one.
            opened=$(open_session "$checkout" "$b" "$launcher" "n$n")
            commit_in "${opened#* }" "src/$b.txt" "$b" "feat: $b"
            dirname "${opened#* }" >>"$fixture/live.txt"
            ;;
        no)
            worked "$checkout" "$b" "$launcher" "n$n" "src/$b.txt" "$b" "feat: $b"
            ;;
        unknown)
            # A retry landed under the same subject with different content: the base's
            # history says it took this change and its content says otherwise.
            worked "$checkout" "$b" "$launcher" "n$n" "src/$b.txt" "mine" "feat: retry $b"
            worked "$checkout" "$b-other" "$launcher" "n$n-other" "src/$b.txt" "theirs" "feat: retry $b"
            land "$checkout" "$b-other"
            ;;
        in-part)
            worked "$checkout" "$b" "$launcher" "n$n" "src/$b.txt" "one" "feat: $b"
            land "$checkout" "$b"
            worked "$checkout" "$b" "$launcher" "n$n-more" "src/$b-more.txt" "two" "feat: $b more"
            ;;
        *) die "no scenario named $state_name" ;;
    esac
    expect "$k" "$b" "$state_name" "$launcher"
}

states=(landed retirable superseded live no unknown in-part)

build_identity() {
    local k=$1 labelled=$2 history=$3 kept=$4 origin seed checkout n i made opened
    origin="$fixture/origins/r$k.git"
    seed="$fixture/seeds/r$k"
    checkout="$fixture/checkouts/r$k"
    mkdir -p "$seed"
    fx_git -C "$seed" init -q -b main
    for i in $(seq 1 40); do
        mkdir -p "$seed/lib"
        printf 'module %s\n%s\n' "$i" "$(seq 1 50)" >"$seed/lib/m$i.txt"
    done
    printf 'target/\n.logs/\n' >"$seed/.gitignore"
    fx_git -C "$seed" add -A
    fx_git -C "$seed" commit -q -m "chore: seed r$k"
    fx_git init -q --bare "$origin"
    fx_git -C "$seed" push -q "$origin" main
    rm -rf "$seed"
    fx_git clone -q "$origin" "$checkout"
    fx register "$checkout" >/dev/null || die "register $checkout failed" 1

    # The swept history: sessions whose records `sweep` forgets and whose streams stay.
    for n in $(seq 1 "$history"); do
        if [ $((n % 5)) -eq 0 ]; then
            worked "$checkout" "old/r$k-$n" "fixture-launcher-gone" "h$n" "src/old-$n.txt" "$n" "feat: old $n"
            land "$checkout" "old/r$k-$n"
        else
            opened=$(open_session "$checkout" "old/r$k-$n" "fixture-launcher-gone" "h$n")
            close_session "${opened%% *}"
        fi
    done
}

build_kept() {
    local k=$1 labelled=$2 kept=$3 checkout n made
    checkout="$fixture/checkouts/r$k"
    # The kept records: the measured launcher's scenarios, then the other managers'.
    made=0
    if [ "$labelled" -gt 0 ]; then
        # Two states in rotation, so every state is reached once seven identities are,
        # and one more landed branch: most of what a manager leaves has landed.
        for n in 0 1; do
            scenario "$checkout" "$k" "$n" "${states[$(((2 * labelled + n) % 7))]}" "$measured_launcher"
            made=$((made + 2))
        done
        scenario "$checkout" "$k" 2 landed "$measured_launcher"
        made=$((made + 1))
    fi
    n=10
    while [ "$made" -lt "$kept" ]; do
        if [ $((n % 3)) -eq 0 ]; then
            worked "$checkout" "keep/r$k-$n" "fixture-launcher-$((n % 4))" "k$n" "src/keep-$n.txt" "$n" "feat: keep $n"
        else
            worked "$checkout" "keep/r$k-$n" "fixture-launcher-$((n % 4))" "k$n" "src/keep-$n.txt" "$n" "feat: keep $n"
            land "$checkout" "keep/r$k-$n"
        fi
        made=$((made + 1))
        n=$((n + 1))
    done
}

build_fixture() {
    local k jobs=0 parallel
    parallel=${RECOVERABLE_FLOOR_JOBS:-16}
    [[ "$parallel" =~ ^[1-9][0-9]*$ ]] || die "RECOVERABLE_FLOOR_JOBS must be a positive integer"
    if [ -e "$fixture" ] && [ ! -f "$fixture/fixture.env" ] &&
        [ ! -f "$fixture/.recoverable-floor-fixture" ] &&
        [ -n "$(find "$fixture" -mindepth 1 -maxdepth 1 -print -quit)" ]; then
        die "$fixture is not an owned fixture; choose an empty scratch directory" 1
    fi
    rm -rf "$fixture"
    mkdir -p "$fixture/home" "$fixture/origins" "$fixture/seeds" "$fixture/checkouts" "$fixture/logs"
    : >"$fixture/.recoverable-floor-fixture"
    : >"$fixture/live.txt"
    printf '[user]\n\tname = Fixture\n\temail = fixture@example.invalid\n[init]\n\tdefaultBranch = main\n[commit]\n\tgpgsign = false\n[advice]\n\tdetachedHead = false\n[maintenance]\n\tauto = false\n' \
        >"$fixture/.gitconfig"
    printf 'version: 1\nrules: []\ndefault: {publication: local-direct, approvals: none}\n' \
        >"$fixture/home/rules.yml"
    printf 'version: 1\ndefault: {pool: 2, overflow: unlimited, delete: [".logs/"]}\n' \
        >"$fixture/home/workspaces.yml"
    # Sweep only after every history writer has finished and before opening any kept
    # session: a global sweep racing another identity can reclaim its active worktree.
    for phase in history kept; do
        for k in $(seq 1 "$identities"); do
            local mine=0
            [ "$k" -le "$labelled" ] && mine=$k
            if [ "$phase" = history ]; then
                (build_identity "$k" "$mine" "$history" "$kept" >"$fixture/logs/r$k.log" 2>&1 || { cat "$fixture/logs/r$k.log" >&2; exit 1; }) &
            else
                (build_kept "$k" "$mine" "$kept" >>"$fixture/logs/r$k.log" 2>&1 || { cat "$fixture/logs/r$k.log" >&2; exit 1; }) &
            fi
            jobs=$((jobs + 1))
            if [ "$jobs" -ge "$parallel" ]; then
                wait -n || die "an identity failed to build; see $fixture/logs/" 1
                jobs=$((jobs - 1))
            fi
        done
        while [ "$jobs" -gt 0 ]; do
            wait -n || die "an identity failed to build; see $fixture/logs/" 1
            jobs=$((jobs - 1))
        done
        if [ "$phase" = history ]; then
            fx sweep --min-age-hours 0 >"$fixture/logs/sweep.log" 2>&1 || {
                cat "$fixture/logs/sweep.log" >&2
                die "history sweep failed; see $fixture/logs/sweep.log" 1
            }
        fi
    done
    finish_fixture
}

# Historical records are disposable fixture setup output; keep their real event
# streams, as on the host after reclamation, but do not count them as kept sessions.
finish_fixture() {
    local record
    for record in "$fixture"/home/sessions/*.json; do
        if grep -q '"launcher": *"fixture-launcher-gone"' "$record"; then
            rm "$record"
        fi
    done
    {
        echo "shape=$shape"
        echo "identities=$identities"
        echo "launcher=$measured_launcher"
        echo "records=$(find "$fixture/home/sessions" -name '*.json' | wc -l)"
        echo "labelled=$(grep -l "\"launcher\": *\"$measured_launcher\"" "$fixture"/home/sessions/*.json | wc -l)"
        echo "streams=$(find "$fixture/home/streams" -name '*.ndjson' | wc -l)"
        echo "built_by=$("$onevcs_bin" --version)"
    } >"$fixture/fixture.env"
    cat "$fixture"/expected.*.tsv >"$fixture/expected.tsv" 2>/dev/null || : >"$fixture/expected.tsv"
}

cmd_fixture() {
    local scale=1 dir="" value
    identities="" labelled="" kept=19 history=60
    while [ $# -gt 0 ]; do
        case "$1" in
            --scale | --dir | --identities | --labelled | --kept | --history)
                [ $# -ge 2 ] || die "fixture: $1 takes a value"
                value=$2
                case "$1" in
                    --scale) scale=$value ;;
                    --dir) dir=$value ;;
                    --identities) identities=$value ;;
                    --labelled) labelled=$value ;;
                    --kept) kept=$value ;;
                    --history) history=$value ;;
                esac
                shift 2
                ;;
            -h | --help) usage; exit 0 ;;
            *) die "fixture: unknown argument $1" ;;
        esac
    done
    [[ "$scale" =~ ^[1-9][0-9]*$ ]] || die "fixture: --scale takes a positive integer, got $scale"
    identities=${identities:-$((20 * scale))}
    labelled=${labelled:-$((10 * scale))}
    for value in "$identities" "$labelled" "$kept" "$history"; do
        [[ "$value" =~ ^[0-9]+$ ]] || die "fixture: a shape value must be a count, got $value"
    done
    [ "$identities" -gt 0 ] || die "fixture: --identities must be positive"
    [ "$labelled" -le "$identities" ] || die "fixture: --labelled $labelled exceeds --identities $identities"
    [ -x "$onevcs_bin" ] || die "no onevcs binary at $onevcs_bin; run 'just run --version' and set ONEVCS_BIN=target/debug/onevcs"
    shape="v3,identities=$identities,labelled=$labelled,kept=$kept,history=$history"
    fixture=${dir:-$(fixture_dir "$scale")}
    if [ -f "$fixture/fixture.env" ] && {
        grep -qx "shape=$shape" "$fixture/fixture.env" ||
        grep -qx "shape=${shape/v3,/v2,}" "$fixture/fixture.env";
    }; then
        finish_fixture
        echo "fixture: reusing $fixture ($(tr '\n' ' ' <"$fixture/fixture.env"))"
        return
    fi
    local started=$SECONDS
    build_fixture
    echo "fixture: built $fixture in $((SECONDS - started))s ($(tr '\n' ' ' <"$fixture/fixture.env"))"
}

held=()
loaders=()

stop_own() {
    local pid
    for pid in "${held[@]}"; do kill "$pid" 2>/dev/null || true; done
    for pid in "${loaders[@]}"; do kill -- "-$pid" 2>/dev/null || true; done
    for pid in "${held[@]}" "${loaders[@]}"; do wait "$pid" 2>/dev/null || true; done
    held=()
    loaders=()
}
trap stop_own EXIT

# Hold every live session's run-root lease the way a working session holds it.
hold_live_leases() {
    local run_root lock
    [ -s "$fixture/live.txt" ] || return 0
    mkdir -p "$fixture/home/locks"
    while read -r run_root; do
        lock="$fixture/home/locks/$(printf 'run:%s' "$run_root" | sha256sum | cut -c1-64).lock"
        # Non-blocking: a lease somebody else already holds is a live session already,
        # and a holder must never keep a caller's output pipe open.
        (exec 9>"$lock" && flock -n -x 9 && exec sleep 86400) </dev/null >/dev/null 2>&1 &
        held+=("$!")
    done <"$fixture/live.txt"
    sleep 0.5
}

start_load() {
    local workers=$1 i warmup=${RECOVERABLE_FLOOR_LOAD_WARMUP:-60}
    [[ "$warmup" =~ ^[0-9]+$ ]] || die "RECOVERABLE_FLOOR_LOAD_WARMUP must be whole seconds"
    for i in $(seq 1 "$workers"); do
        setsid bash -c 'while :; do head -c 20000000 /dev/urandom | gzip -1 >/dev/null; '"$real_git"' -C "$0" log -p --all >/dev/null 2>&1; done' \
            "$repo_root" >/dev/null 2>&1 &
        loaders+=("$!")
    done
    sleep "$warmup"
}

load1() { cut -d' ' -f1 /proc/loadavg; }

# The command line for one read, as an array assigned to `argv`, and its stdin in `feed`.
# Encode every character representable in a shell argument as a JSON string.
json_string() {
    local text=$1 number octal char escaped
    text=${text//\\/\\\\}
    text=${text//\"/\\\"}
    for ((number = 1; number < 32; number++)); do
        printf -v octal '\\%03o' "$number"
        printf -v char '%b' "$octal"
        printf -v escaped '\\u%04x' "$number"
        text=${text//"$char"/"$escaped"}
    done
    printf '"%s"' "$text"
}

read_command() {
    local read=$1
    feed=""
    case "$read" in
        v0.42.0) argv=("$baseline" recoverable --json --label "launcher=$session") ;;
        legacy | prototype | decision) argv=("$onevcs_bin" recoverable --json --label "launcher=$session") ;;
        stop-verdict)
            argv=(bash "$aio/scripts/unpublished.sh" --stop-verdict)
            feed="{\"session\":$(json_string "$session"),\"continuation\":false}"
            ;;
        stop-guard) argv=(onepipeline stop-guard --session "$session" --format neutral) ;;
        onevcs-version) argv=("$baseline" --version) ;;
        onepipeline-version) argv=(onepipeline --version) ;;
        *) die "run: no read named $read" ;;
    esac
}

read_env() {
    local read=$1
    env_args=()
    case "$read" in
        prototype | decision) env_args+=("ONEVCS_SPIKE_RECOVERABLE=$read" "ONEVCS_SPIKE_CACHE_DIR=$cache") ;;
        legacy) env_args+=("ONEVCS_SPIKE_RECOVERABLE=legacy") ;;
    esac
    if [ -n "$fixture" ]; then
        env_args+=("HOME=$fixture" "ONEVCS_HOME=$fixture/home")
    else
        # A read must not let git status refresh an index inside the real registry.
        env_args+=("GIT_OPTIONAL_LOCKS=0")
    fi
}

once() {
    local path=$1 out=$2
    local status=0
    (cd / && env "${env_args[@]}" PATH="$path" "${argv[@]}" <<<"$feed" >"$out" 2>"$out.err") || status=$?
    # The guard's block verdict exits 1; every other nonzero exit is a failed read.
    if [ "$status" -ne 0 ] && ! { [ "$read" = stop-guard ] && [ "$status" -eq 1 ] && grep -q '"verdict"' "$out"; }; then
        cat "$out.err" >&2
        die "$read exited $status; repair the read before measuring it again" 1
    fi
}

cmd_run() {
    local read="" scale=1 real=0 runs=10 cold=0 workers=0 count_runs=1 dispatches=0 given="" profile=0
    session="" baseline=$(command -v onevcs || true) aio=${AIO_CHECKOUT:-$HOME/ai-orchestrator}
    while [ $# -gt 0 ]; do
        case "$1" in
            --scale | --dir | --runs | --session | --load | --count-runs | --baseline | --aio)
                [ $# -ge 2 ] || die "run: $1 takes a value"
                ;;
        esac
        case "$1" in
            --scale) scale=$2; shift 2 ;;
            --dir) given=$2; shift 2 ;;
            --real) real=1; shift ;;
            --runs) runs=$2; shift 2 ;;
            --cold) cold=1; shift ;;
            --session) session=$2; shift 2 ;;
            --load) workers=$2; shift 2 ;;
            --count-runs) count_runs=$2; shift 2 ;;
            --dispatches) dispatches=1; shift ;;
            --profile) profile=1; shift ;;
            --baseline) baseline=$2; shift 2 ;;
            --aio) aio=$2; shift 2 ;;
            -h | --help) usage; exit 0 ;;
            -*) die "run: unknown argument $1" ;;
            *) read=$1; shift ;;
        esac
    done
    [ -n "$read" ] || die "run: name a read (v0.42.0, legacy, prototype, decision, stop-verdict, stop-guard, onevcs-version, onepipeline-version)"
    [[ "$runs" =~ ^[1-9][0-9]*$ ]] || die "run: --runs takes a positive integer, got $runs"
    [[ "$count_runs" =~ ^[0-9]+$ ]] || die "run: --count-runs takes a count, got $count_runs"
    [[ "$workers" =~ ^[0-9]+$ ]] || die "run: --load takes a worker count, got $workers"
    fixture=""
    if [ "$real" -eq 1 ]; then
        [ -n "$session" ] || die "run --real: name the manager session with --session"
        cache="$state/real-cache/v2"
        target="real"
    else
        if [ -n "$given" ]; then
            [ -f "$given/fixture.env" ] || die "run: $given holds no fixture; build one with 'fixture --dir $given'"
            fixture=$given
            target="fixture-$(basename "$given")"
        else
            cmd_fixture --scale "$scale" >&2
            fixture=$(fixture_dir "$scale")
            target="fixture-${scale}x"
        fi
        cache="$fixture/home/cache/recoverable/v2"
        [ -n "$session" ] || session=$(sed -n 's/^launcher=//p' "$fixture/fixture.env")
        hold_live_leases
    fi
    case "$read" in
        v0.42.0 | onevcs-version)
            [ -x "$baseline" ] || die "run: no baseline onevcs; pass --baseline PATH"
            ;;
        stop-verdict)
            [ -f "$aio/scripts/unpublished.sh" ] || die "run: no ai-orchestrator checkout at $aio; pass --aio PATH"
            ;;
    esac
    [ -x "$onevcs_bin" ] || die "no onevcs binary at $onevcs_bin; run 'just run --version' and set ONEVCS_BIN=target/debug/onevcs"
    read_command "$read"
    read_env "$read"
    [ "$profile" -eq 1 ] && env_args+=("ONEVCS_SPIKE_PROFILE=1")
    local scratch counter shim_dir dispatch_count="" i started ended times=() counts=()
    mkdir -p "$state"
    scratch=$(mktemp -d "$state/measurement.XXXXXX")
    shim_dir="$scratch/shim"
    counter="$scratch/count"
    mkdir -p "$shim_dir"
    printf '#!/bin/sh\nprintf x >>"%s"\nexec "%s" "$@"\n' "$counter" "$real_git" >"$shim_dir/git"
    chmod +x "$shim_dir/git"
    if [ "$dispatches" -eq 1 ]; then
        dispatch_count=$(timeout 300 onepipeline host 2>/dev/null |
            grep -cE '^  [^ ]+ [^ ]+ +[a-z-]+ +[0-9hms]+$' || true)
    fi
    [ "$workers" -gt 0 ] && start_load "$workers"
    local fresh=0
    case "$read" in prototype | decision) fresh=1 ;; esac
    # One unmeasured run primes a warm read; a cold one empties the cache every time.
    if [ "$fresh" -eq 1 ] && [ "$cold" -eq 0 ]; then once "$PATH" "$scratch/prime"; fi
    local load_start load_end
    load_start=$(load1)
    for i in $(seq 1 "$runs"); do
        [ "$cold" -eq 1 ] && rm -rf "$cache"
        started=$EPOCHREALTIME
        once "$PATH" "$scratch/out"
        ended=$EPOCHREALTIME
        times+=("$(awk -v a="$started" -v b="$ended" 'BEGIN { printf "%.1f", (b - a) * 1000 }')")
    done
    load_end=$(load1)
    for i in $(seq 1 "$count_runs"); do
        [ "$cold" -eq 1 ] && rm -rf "$cache"
        : >"$counter"
        once "$shim_dir:$PATH" "$scratch/counted"
        counts+=("$(wc -c <"$counter" | tr -d ' ')")
    done
    stop_own
    local stats spawns commit
    stats=$(printf '%s\n' "${times[@]}" | sort -n | awk '
        { v[NR] = $1 }
        END {
            p90 = int((NR * 9 + 9) / 10); if (p90 > NR) p90 = NR
            med = (NR % 2) ? v[(NR + 1) / 2] : (v[NR / 2] + v[NR / 2 + 1]) / 2
            printf "median_ms=%.1f p90_ms=%.1f max_ms=%.1f", med, v[p90], v[NR]
        }')
    if [ "${#counts[@]}" -gt 0 ]; then
        spawns=$(printf '%s\n' "${counts[@]}" | sort -n | sed -n '1p;$p' | paste -sd- | sed 's/^\([0-9]*\)-\1$/\1/')
    else
        spawns="not-counted"
    fi
    commit=$(git -C "$repo_root" rev-parse --short HEAD)
    git -C "$repo_root" diff --quiet HEAD -- || commit="$commit+dirty"
    printf 'read=%s target=%s cache=%s runs=%s %s git_spawns=%s load1_start=%s load1_end=%s load=%s dispatches=%s rows=%s commit=%s\n' \
        "$read" "$target" "$([ "$fresh" -eq 1 ] && { [ "$cold" -eq 1 ] && echo cold || echo warm; } || echo none)" \
        "$runs" "$stats" "$spawns" "$load_start" "$load_end" \
        "$([ "$workers" -gt 0 ] && echo "generated:$workers" || echo ambient)" \
        "${dispatch_count:-not-counted}" "$(rows_of "$scratch/out")" "$commit"
    if [ "$profile" -eq 1 ]; then
        echo "profile=$(sed -n 's/^onevcs-spike-profile //p' "$scratch/out.err" | tail -1)"
    fi
    rm -rf "$scratch"
}

# How many rows a recoverable answer held, or what a verdict said.
rows_of() {
    local out=$1
    case "$(head -c 1 "$out" 2>/dev/null)" in
        "[") grep -o '"stopped_because":' "$out" | wc -l | tr -d ' ' ;;
        "{") grep -o '"verdict": *"[a-z]*"' "$out" | head -1 | sed 's/.*"\([a-z]*\)"$/\1/' ;;
        *) echo "-" ;;
    esac
}

case "${1:-}" in
    fixture) shift; cmd_fixture "$@" ;;
    run) shift; cmd_run "$@" ;;
    -h | --help | "") usage ;;
    *) die "unknown verb $1; the verbs are 'fixture' and 'run'" ;;
esac
