#!/usr/bin/env bash
# Self-contained test for the Continuous-Audit trigger scaffold (2630b4c5).
#
# Exercises `scripts/continuous-audit.sh`:
#   * --help exits 0 and describes the loop,
#   * --dry-run records NOTHING (the audit ledger stays absent) yet prints a
#     plan + metrics,
#   * the record path (real run) ingests a CONFIRMED finding into the review
#     queue AND appends a round to the convergence ledger, so a subsequent
#     `overwatch audit-metrics --json` reports that round.
#
# Runs with a sandboxed HOME + cwd so nothing real is touched. Pin the binary
# with OVERWATCH_BIN (the cargo wrapper test sets it to target/debug/overwatch).
#
# Exit 0 on success, non-zero on any failed assertion.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$REPO/scripts/continuous-audit.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

# --- locate / build the overwatch binary -------------------------------------
OW=""
if [ -n "${OVERWATCH_BIN:-}" ] && [ -x "${OVERWATCH_BIN}" ]; then
  OW="$OVERWATCH_BIN"
elif command -v overwatch >/dev/null 2>&1; then
  OW="$(command -v overwatch)"
elif [ -x "$REPO/target/release/overwatch" ]; then
  OW="$REPO/target/release/overwatch"
elif [ -x "$REPO/target/debug/overwatch" ]; then
  OW="$REPO/target/debug/overwatch"
else
  echo "building overwatch binary for the test..."
  ( . "$HOME/.cargo/env" 2>/dev/null || true
    cargo build -p overwatch --bin overwatch >/dev/null 2>&1 )
  OW="$REPO/target/debug/overwatch"
fi
[ -x "$OW" ] || fail "could not find or build the overwatch binary"
echo "using overwatch: $OW"

# --- sandbox HOME + cwd (nothing real touched) -------------------------------
TMP="$(mktemp -d "${TMPDIR:-/tmp}/continuous-audit.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT
SANDBOX_HOME="$TMP/home"
WORK="$TMP/work"
mkdir -p "$SANDBOX_HOME" "$WORK"

# Run continuous-audit.sh from $WORK (so the sandbox project key is derived
# there), capturing stdout+stderr into $OUT and the exit code into $RC in the
# PARENT shell (the `cd` is inside the command substitution, not a wrapping
# subshell, so $OUT/$RC survive for the assertions below).
run_ca() {
  OUT="$(
    cd "$WORK" && OVERWATCH_BIN="$OW" HOME="$SANDBOX_HOME" \
    bash "$SCRIPT" "$@" 2>&1
  )"
  RC=$?
}
run_ow() {
  ( cd "$WORK" && OVERWATCH_BIN="$OW" HOME="$SANDBOX_HOME" \
    "$OW" "$@" )
}

echo
echo "=== case 1: --help exits 0 and describes the loop ==="
run_ca --help
[ "$RC" -eq 0 ] || fail "--help should exit 0 (got $RC)"
grep -qi "Continuous-Audit" <<<"$OUT" || fail "--help should mention Continuous-Audit"
pass "--help works"

echo
echo "=== case 2: --dry-run records nothing but prints plan + metrics ==="
run_ca --dry-run --round 1 --target specguard \
    --new-findings 3 --confirmed 2 --regression-tests-added 1 \
    --finding 'D-001|high|dry finding|crates/specguard/src/x.rs'
[ "$RC" -eq 0 ] || fail "--dry-run should exit 0 (got $RC)"
grep -qi "DRY RUN" <<<"$OUT" || fail "--dry-run should announce it is a dry run"
grep -q "D-001" <<<"$OUT" || fail "--dry-run should list the finding"
# Ledger must NOT have been written.
LEDGER="$SANDBOX_HOME/.overwatch"
if find "$LEDGER" -name audit_rounds.jsonl 2>/dev/null | grep -q .; then
  fail "--dry-run must NOT write the audit ledger"
fi
pass "--dry-run is side-effect free"

echo
echo "=== case 3: record path ingests finding + appends a round ==="
run_ca --round 2026W28 --target specguard,stuckguard \
    --new-findings 4 --confirmed 3 --regression-tests-added 3 \
    --finding 'R-007|high|confirmed real finding|crates/specguard/src/y.rs'
[ "$RC" -eq 0 ] || fail "record run should exit 0 (got $RC)"
grep -q "round 2026W28 recorded" <<<"$OUT" || fail "record run should confirm the round was recorded"

# The finding must surface in the review queue.
QUEUE="$( run_ow review-queue --json )"
grep -q "R-007" <<<"$QUEUE" || fail "confirmed finding R-007 must appear in review-queue"
pass "confirmed finding reached the review queue"

# The round must appear in the metrics ledger. The round-id is a free-form
# string (e.g. an ISO week), so the JSON reports it as a quoted string.
METRICS="$( run_ow audit-metrics --json )"
grep -q '"round": "2026W28"' <<<"$METRICS" || fail "round 2026W28 must appear in audit-metrics"
grep -q '"new_findings": 4' <<<"$METRICS" || fail "round 2026W28 new_findings must be recorded"
pass "round metrics reached the convergence ledger"

echo
echo "=== case 4: a round that confirms findings but closes none is REFUSED ==="
# The defect this closes (backlog 5b33b4cd): the ledger proved the audit was
# finding without preventing -- 2026W29 recorded confirmed:10 with
# regression_tests_added:0. Accepting such a round lets the same defect class be
# re-harvested next round forever, which is what `converging:false` was
# reporting. An unclosed round is now not recordable.
run_ca --round 2026W30 --target specguard \
    --new-findings 5 --confirmed 4 --regression-tests-added 1 \
    --finding 'U-001|high|confirmed but never closed|crates/specguard/src/z.rs'
[ "$RC" -ne 0 ] || fail "case 4: a round with regression_tests_added < confirmed must NOT exit 0 (got $RC)"
grep -qi "regression" <<<"$OUT" || fail "case 4: the refusal must name what was missing"

# Refusing must mean refusing: the round must be absent from the ledger, not
# recorded-and-warned. A warning that still writes is the fail-open this closes.
METRICS4="$( run_ow audit-metrics --json )"
grep -q '"round": "2026W30"' <<<"$METRICS4" && fail "case 4: a refused round must NOT reach the ledger"

# And the script must not sign off on a run it just failed. `run_ow` swallowed
# every non-zero exit, so before this fix the binary could refuse while the
# script still printed its PASS line -- an inert gate.
grep -q "round 2026W30 recorded" <<<"$OUT" && fail "case 4: the script claimed success for a refused round"
pass "an unclosed round is refused, absent from the ledger, and not signed off"

echo
echo "=== case 5: ANTI-VACUITY — a fully closed round still records ==="
# Without this, 'refuse everything' would satisfy case 4. Equal counts are the
# accepting case: closure_rate 1.0.
run_ca --round 2026W31 --target specguard \
    --new-findings 2 --confirmed 2 --regression-tests-added 2 \
    --finding 'C-001|high|confirmed and closed|crates/specguard/src/w.rs'
[ "$RC" -eq 0 ] || fail "case 5: a fully closed round must still be accepted (got $RC)"
METRICS5="$( run_ow audit-metrics --json )"
grep -q '"round": "2026W31"' <<<"$METRICS5" || fail "case 5: an accepted round must reach the ledger"
pass "a fully closed round is still accepted and recorded"

echo
echo "PASS: continuous-audit.sh help/dry-run/record paths all behave,"
echo "      and an unclosed round is refused end-to-end."
