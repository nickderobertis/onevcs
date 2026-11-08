#!/usr/bin/env bash
# The invocations behind the report's figures, in the order they were taken. Each
# llmlint: ignore-file[changed_behavior_has_e2e] the order the report's figures were taken in, each step a measure.mjs call harness.test.mjs drives; this measurement code lives only on the preserved spike branch, which the plan's run discards once it succeeds (orchestration.md, Spikes), and is never merged into the repository's tree.
# writes one measure.mjs document to "$OUT/<name>.json".
#
#   spikes/unpublished-costs/matrix.sh <phase> OUT BINARY [SESSION BRANCH REPO]
#
# BINARY is the engine built by build.sh. Phases: real (this host's real state),
# prototypes (each prototype over real state), record (the decision record over a
# scratch copy of ONEVCS_HOME, named by $HOME_COPY), fixture (the generated fixtures,
# named by $FIXTURE_ONE and $FIXTURE_HOST), strategies (each read under each strategy
# over real state), staged (the warm read over a staged record).
set -euo pipefail
phase=$1 out=$2 binary=$3 session=${4:-} branch=${5:-} repo=${6:-}
here=$(cd "$(dirname "$0")" && pwd)
ack="${XDG_STATE_HOME:-$HOME/.local/state}/ai-orchestrator/stop-unfinished/acknowledged"
mkdir -p "$out"
failed=0
# Each measure runs whatever the one before it answered, so one that fails is named with
# its own last diagnostic, and the matrix exits 1 once the rest have run.
m() {
  local name=$1; shift
  if ! node "$here/measure.mjs" --binary "$binary" "$@" > "$out/$name.json" 2> "$out/$name.err"; then
    echo "matrix: measure $name failed: $(tail -1 "$out/$name.err" 2>/dev/null || echo 'no diagnostic'); its document is $out/$name.json" >&2
    failed=1
  fi
}
reads() { # prefix, extra args
  local prefix=$1; shift
  m "$prefix-host" --measure host --acknowledgements "$ack" --rows-out "$out/$prefix-host.rows.json" "$@"
  m "$prefix-session" --measure session --session "$session" --acknowledgements "$ack" "$@"
  m "$prefix-stop-guard" --measure stop-guard --session "$session" --acknowledgements "$ack" "$@"
  m "$prefix-acknowledge" --measure acknowledge --session "$session" --branch "$branch" --repo "$repo" --acknowledgements "$ack" "$@"
}
case $phase in
  real)
    m real-host-counted --measure host --acknowledgements "$ack" --no-trace --count
    reads real
    ;;
  prototypes)
    for prototype in local-head no-paths native-unbounded local-head,no-paths,native-unbounded; do
      m "proto-$prototype-host" --measure host --acknowledgements "$ack" --prototype "$prototype" \
        --rows-out "$out/proto-$prototype-host.rows.json"
    done
    reads combined --prototype local-head,no-paths,native-unbounded
    ;;
  record)
    : "${HOME_COPY:?HOME_COPY names the scratch copy of ONEVCS_HOME}"
    m record-cold-unmodified --measure host --acknowledgements "$ack" --onevcs-home "$HOME_COPY" \
      --rows-out "$out/record-unmodified.rows.json"
    m record-cold --measure record-write --acknowledgements "$ack" --onevcs-home "$HOME_COPY" \
      --prototype decision-record --baseline-rows "$out/record-unmodified.rows.json"
    m record-warm --measure host-warm --acknowledgements "$ack" --onevcs-home "$HOME_COPY" \
      --prototype decision-record --baseline-rows "$out/record-unmodified.rows.json"
    ;;
  fixture)
    for which in one host; do
      var=FIXTURE_${which^^}; home=${!var:?$var names a built fixture home}
      launcher=$(cat "$home/../launcher" 2>/dev/null || echo fixture-launcher-measured)
      m "fixture-$which-host" --measure host --onevcs-home "$home" --acknowledgements "$out/empty-ack"
      m "fixture-$which-recoverable" --measure recoverable-host --onevcs-home "$home" --onevcs "${ONEVCS_BIN:-onevcs}"
      m "fixture-$which-session" --measure session --session "$launcher" --onevcs-home "$home" --acknowledgements "$out/empty-ack"
      m "fixture-$which-stop-guard" --measure stop-guard --session "$launcher" --onevcs-home "$home" --acknowledgements "$out/empty-ack"
    done
    ;;
  strategies)
    # Each read under each strategy, a host or session read compared with an unmodified
    # read of the same state taken immediately before it.
    all=local-head,no-paths,batch-cat-file,stdin-lists
    for prototype in local-head no-paths batch-cat-file stdin-lists native-unbounded "$all"; do
      name=${prototype//,/+}; [ "$prototype" = "$all" ] && name=all-but-native
      m "before-$name-host" --measure host --acknowledgements "$ack" --no-trace --rows-out "$out/before-$name-host.rows.json"
      m "$name-host" --measure host --acknowledgements "$ack" --prototype "$prototype" --baseline-rows "$out/before-$name-host.rows.json" --count
      m "before-$name-session" --measure session --session "$session" --acknowledgements "$ack" --no-trace --rows-out "$out/before-$name-session.rows.json"
      m "$name-session" --measure session --session "$session" --acknowledgements "$ack" --prototype "$prototype" --baseline-rows "$out/before-$name-session.rows.json"
      m "$name-stop-guard" --measure stop-guard --session "$session" --acknowledgements "$ack" --prototype "$prototype"
      m "$name-acknowledge" --measure acknowledge --session "$session" --branch "$branch" --repo "$repo" --acknowledgements "$ack" --prototype "$prototype"
    done
    ;;
  staged)
    # The warm read over a scratch copy whose record is staged by record-gains.mjs.
    : "${HOME_COPY:?HOME_COPY names the scratch copy of ONEVCS_HOME}"
    : "${STAGED:?STAGED names the record record-gains.mjs staged}"
    : "${PRISTINE:?PRISTINE names the record as a cold read wrote it}"
    for prototype in decision-record decision-record,local-head,no-paths decision-record,local-head,no-paths,batch-cat-file; do
      name=${prototype//,/+}
      m "before-staged-$name" --measure host --acknowledgements "$ack" --onevcs-home "$HOME_COPY" --no-trace --rows-out "$out/before-staged-$name.rows.json"
      BASELINE=$out/before-staged-$name.rows.json
      m "staged-warm-$prototype" --measure host --acknowledgements "$ack" --onevcs-home "$HOME_COPY" --prototype "$prototype" --stage-record "$STAGED" --baseline-rows "$BASELINE"
      m "pristine-warm-$prototype" --measure host --acknowledgements "$ack" --onevcs-home "$HOME_COPY" --prototype "$prototype" --stage-record "$PRISTINE" --baseline-rows "$BASELINE"
    done
    m record-acknowledge --measure acknowledge --session "$session" --branch "$branch" --repo "$repo" --acknowledgements "$ack" --onevcs-home "$HOME_COPY" --prototype decision-record --stage-record "$PRISTINE"
    ;;
  *) echo "matrix.sh: unknown phase $phase; the phases are real, prototypes, record, staged, strategies and fixture" >&2; exit 2 ;;
esac
exit "$failed"
