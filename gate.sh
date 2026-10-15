#!/usr/bin/env bash
# Phase gate for the Metro to Rust port. See RULES.md.
#
#   ./gate.sh                 run every gate for the current phase
#   ./gate.sh --quick         faster inner loop: one perf run, cold builds only when caches are empty
#   ./gate.sh --only compile  run one gate (compile, tests or perf)
#   ./gate.sh --report        full run that also writes port/reports/phase-<n>.md
#   ./gate.sh --phase 2       pick the phase instead of the first unapproved one
#
# Gates run in order and stop at the first failure:
#   1. compile  cargo fmt, cargo clippy -D warnings, napi build (fast profile), binding loads, flow, eslint
#   2. tests    cargo test, full Jest suite with JS, Jest with the phase's components
#               in Rust (minus port/js-only-tests.json), differential bundle check
#   3. perf     release napi build, then pure-JS baseline against this checkout (port/bench/compare.js)
#
# Exit codes: 0 all gates passed, 10 compile, 20 tests, 30 perf, 2 blocked or misuse.
# Cargo paths in error messages are relative to metro-rs/.
# Full logs are in port/.gate/<gate>.log, a summary in port/.gate/result.json.

set -uo pipefail
cd "$(dirname "$0")"
ROOT=$PWD
OUT=$ROOT/port/.gate
PHASE=""
ONLY=""
QUICK=0
REPORT=0

while [ $# -gt 0 ]; do
  case "$1" in
    --phase) PHASE=$2; shift 2 ;;
    --only) ONLY=$2; shift 2 ;;
    --quick) QUICK=1; shift ;;
    --report) REPORT=1; shift ;;
    -h|--help) sed -n '2,/^$/p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown option $1 (see ./gate.sh --help)"; exit 2 ;;
  esac
done

if [ "$REPORT" = 1 ] && { [ "$QUICK" = 1 ] || [ -n "$ONLY" ]; }; then
  echo "--report needs a full run: drop --quick and --only."; exit 2
fi

if [ "$REPORT" = 1 ] && [ -n "$(git status --porcelain)" ]; then
  echo "--report needs a clean, committed tree so the report names a real commit. Commit first."; exit 2
fi

[ -z "$PHASE" ] && PHASE=$(node port/lib/phase.js current)
if [ "$PHASE" = done ]; then echo "Every phase is approved. Nothing to gate."; exit 0; fi
node port/lib/phase.js check "$PHASE" || exit 2
METRO_RUST_LIST=$(node port/lib/phase.js components "$PHASE")
TITLE=$(node -e "console.log(JSON.parse(process.argv[1]).title)" "$(node port/lib/phase.js json "$PHASE")")
HAS_RUST=0; [ -f metro-rs/Cargo.toml ] && HAS_RUST=1

mkdir -p "$OUT"
rm -f "$OUT"/*.log "$OUT"/result.json
STAGES=()
STARTED=$(date +%s)

say() { printf '\n=== %s\n' "$*"; }
record() { STAGES+=("{\"gate\":\"$1\",\"status\":\"$2\",\"step\":\"$3\"}"); }
write_result() {
  local IFS=,
  printf '{"phase":%s,"title":"%s","metroRust":"%s","quick":%s,"passed":%s,"seconds":%s,"gates":[%s]}\n' \
    "$PHASE" "$TITLE" "$METRO_RUST_LIST" "$([ $QUICK = 1 ] && echo true || echo false)" "$1" \
    "$(( $(date +%s) - STARTED ))" "${STAGES[*]}" > "$OUT/result.json"
}
# step <gate> <name> <command...>: runs a step, logs it, and on failure prints
# the end of its output so the error is visible without opening the log.
step() {
  local gate=$1 name=$2; shift 2
  printf -- '--- %s: %s\n' "$gate" "$name"
  { printf '\n### %s: %s\n$ %s\n' "$gate" "$name" "$*"; } >> "$OUT/$gate.log"
  local tmp; tmp=$(mktemp)
  "$@" > "$tmp" 2>&1
  local code=$?
  cat "$tmp" >> "$OUT/$gate.log"
  if [ $code -ne 0 ]; then
    echo "FAILED: $name (exit $code). Last lines, full output in port/.gate/$gate.log:"
    tail -n "${GATE_TAIL:-80}" "$tmp" | sed 's/^/    /'
    rm -f "$tmp"; FAILED_STEP=$name; return 1
  fi
  rm -f "$tmp"; return 0
}
fail() {
  record "$1" fail "$FAILED_STEP"; write_result false
  echo; echo "GATE FAILED at $1 ($FAILED_STEP). The phase is not done; fix this and run ./gate.sh again."
  exit "$2"
}
want() { [ -z "$ONLY" ] || [ "$ONLY" = "$1" ]; }

echo "Phase $PHASE ($TITLE). Rust components: $METRO_RUST_LIST"
[ $HAS_RUST = 0 ] && echo "Note: metro-rs/Cargo.toml does not exist yet; phase 0 creates it."

# ---------------------------------------------------------------- 1. compile
if want compile; then
  say "1/3 compile"
  if [ $HAS_RUST = 1 ]; then
    step compile "cargo fmt" cargo fmt --manifest-path metro-rs/Cargo.toml --all --check || fail compile 10
    step compile "cargo clippy" cargo clippy --manifest-path metro-rs/Cargo.toml --workspace --all-targets --locked \
      -- -D warnings || fail compile 10
    step compile "native binding build (fast profile)" yarn --cwd metro-rs build:fast || fail compile 10
    step compile "binding loads in Node" node -e "const b = require('./metro-rs'); if (typeof b.version !== 'function') throw new Error('metro-rs must export version()'); console.log('metro-rs', b.version())" || fail compile 10
  else
    FAILED_STEP="metro-rs/Cargo.toml is missing"
    echo "FAILED: there is no Rust workspace. Phase 0 must create metro-rs/ (see RULES.md, 'Repository layout')."
    fail compile 10
  fi
  step compile "flow" yarn --silent typecheck || fail compile 10
  step compile "eslint" yarn --silent lint --quiet || fail compile 10
  record compile pass ""
fi

# ---------------------------------------------------------------- 2. tests
if want tests; then
  say "2/3 tests"
  if [ $HAS_RUST = 1 ]; then
    step tests "cargo test" cargo test --manifest-path metro-rs/Cargo.toml --workspace --locked || fail tests 20
  fi
  step tests "jest, all JavaScript (METRO_RUST=off)" env METRO_RUST=off yarn --silent jest --ci --silent || fail tests 20
  if [ "$METRO_RUST_LIST" != off ]; then
    if [ $HAS_RUST = 0 ]; then
      FAILED_STEP="Rust components switched on but metro-rs/ is missing"; fail tests 20
    fi
    IGNORE=$(node port/lib/phase.js js-only "$METRO_RUST_LIST" | sed 's/[.]/\\./g' | paste -sd'|' -)
    if [ -n "$IGNORE" ]; then
      step tests "jest with METRO_RUST=$METRO_RUST_LIST (and METRO_RUST_VERIFY)" env METRO_RUST="$METRO_RUST_LIST" METRO_RUST_VERIFY="$METRO_RUST_LIST" \
        yarn --silent jest --ci --silent --testPathIgnorePatterns "($IGNORE)" '/node_modules/' || fail tests 20
    else
      step tests "jest with METRO_RUST=$METRO_RUST_LIST (and METRO_RUST_VERIFY)" env METRO_RUST="$METRO_RUST_LIST" METRO_RUST_VERIFY="$METRO_RUST_LIST" \
        yarn --silent jest --ci --silent || fail tests 20
    fi
  fi
  step tests "differential bundles (JS vs Rust, byte for byte)" node port/diff/differential.js --phase "$PHASE" || fail tests 20
  # From phase 4 (incremental graph), the exit criterion includes identical
  # HMR messages across a scripted edit session.
  if [ "$PHASE" -ge 4 ]; then
    step tests "HMR session differential (JS vs Rust)" node port/diff/hmr-session.js --phase "$PHASE" || fail tests 20
  fi
  record tests pass ""
fi

# ---------------------------------------------------------------- 3. perf
if want perf; then
  say "3/3 performance"
  if [ $HAS_RUST = 1 ]; then
    # Benchmarks always measure the fully optimised release build.
    step perf "native binding build (release)" yarn --cwd metro-rs build || fail perf 30
  fi
  PERF_ARGS=(--phase "$PHASE")
  [ $QUICK = 1 ] && PERF_ARGS+=(--quick)
  [ $REPORT = 1 ] && PERF_ARGS+=(--runs 5)
  GATE_TAIL=40 step perf "pure JS vs this checkout" node port/bench/compare.js "${PERF_ARGS[@]}" || {
    cat "$OUT/perf/perf.md" 2>/dev/null; fail perf 30; }
  cat "$OUT/perf/perf.md"
  record perf pass ""
fi

write_result true
echo
if [ -n "$ONLY" ] || [ $QUICK = 1 ]; then
  echo "Selected gates passed. A phase is only done after a full ./gate.sh --report."
  exit 0
fi
if [ $REPORT = 1 ]; then
  node port/lib/report.js "$PHASE" || exit 2
  echo "ALL GATES PASSED. Report written to port/reports/phase-$PHASE.md."
  echo "Commit it, open the phase PR, post the report and STOP: phase $((PHASE + 1)) needs Szymon's approval."
else
  echo "ALL GATES PASSED. Run ./gate.sh --report to produce the phase report."
fi
exit 0
