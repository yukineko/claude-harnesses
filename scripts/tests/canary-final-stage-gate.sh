#!/usr/bin/env bash
# Self-contained test for the canary health-gate's FINAL-STAGE COVERAGE GAP
# (audited in docs/audit-canary-health-gate-coverage.md).
#
# The health-gate block in scripts/rollout-plugins.sh used to sit entirely
# inside `if [ "$s" -lt "$((nstages - 1))" ]`, i.e. it only ran BETWEEN stages.
# Consequences, both measured before this test existed:
#   * a SINGLE-stage canary (`--plugin <one> --canary`) ran the gate ZERO times,
#     yet still applied the stage (copy + registry repoint) and then called
#     run_rebuild_and_sync unconditionally — "checked and healthy" and "never
#     checked" were indistinguishable downstream;
#   * for N stages, the LAST stage was likewise never gated.
#
# This test pins the fixed contract: the gate runs for EVERY stage, including
# the last (or only) one, and its existing exit-code contract is preserved
# (rc=3 => roll the stage back and halt with exit 4).
#
# Case A (single stage, gate rc=3)   -> rollback + exit 4      [RED before fix]
# Case B (2 stages, only the LAST
#         stage's gate returns rc=3) -> rollback + exit 4      [RED before fix]
# Case C (single stage, gate rc=0)   -> proceeds to completion, ANTI-VACUITY:
#         an implementation that simply always halts fails this case.
#
# MAC-RUNNABLE: like canary-gate-eval-failclosed.sh (whose fake-overwatch-stub +
# OVERWATCH_CANARY_SINCE seeding technique this reuses) it never touches the
# real ~/.claude tree; everything runs against a temp cache/registry/HOME under
# mktemp -d, removed on exit. No GNU-only coreutils are used.
#
# Exit 0 on success, non-zero on any failed assertion.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$REPO/scripts/rollout-plugins.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

# --- locate / build the REAL overwatch binary (used for the non-gate subcmds) -
OW=""
if [ -n "${OVERWATCH_BIN:-}" ] && [ -x "${OVERWATCH_BIN}" ]; then
  OW="$OVERWATCH_BIN"
elif [ -x "$REPO/target/debug/overwatch" ]; then
  OW="$REPO/target/debug/overwatch"
elif [ -x "$REPO/target/release/overwatch" ]; then
  OW="$REPO/target/release/overwatch"
elif command -v overwatch >/dev/null 2>&1; then
  OW="$(command -v overwatch)"
else
  echo "building overwatch binary for the test..."
  ( . "$HOME/.cargo/env" 2>/dev/null || true
    cargo build -p overwatch --bin overwatch >/dev/null 2>&1 )
  OW="$REPO/target/debug/overwatch"
fi
[ -x "$OW" ] || fail "could not find or build the overwatch binary"
echo "using overwatch: $OW"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/canary-final-stage.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

COUNTER="$TMP/gate-calls"

# --- stub overwatch -----------------------------------------------------------
# Counts canary-gate invocations in $COUNTER and returns $GATE_FAIL_FROM_CALL-th
# and later calls as rc=3 (ROLLBACK advised). Every other subcommand
# (canary-plan / canary-rollback-plan / record-*) is delegated to the real
# binary so the surrounding machinery is exercised for real.
#   GATE_FAIL_FROM_CALL=1 -> the very first gate call advises rollback
#   GATE_FAIL_FROM_CALL=2 -> the first call is healthy, the second advises it
#   GATE_FAIL_FROM_CALL=0 -> never advise rollback (always healthy)
STUB="$TMP/ow-stub.sh"
cat >"$STUB" <<STUBEOF
#!/usr/bin/env bash
if [ "\$1" = "canary-gate" ]; then
  n=0
  [ -f "$COUNTER" ] && n="\$(cat "$COUNTER")"
  n=\$(( n + 1 ))
  printf '%s' "\$n" > "$COUNTER"
  echo "stub-gate: call \$n"
  if [ "\${GATE_FAIL_FROM_CALL:-0}" != 0 ] && [ "\$n" -ge "\${GATE_FAIL_FROM_CALL}" ]; then
    exit 3
  fi
  exit 0
fi
exec "$OW" "\$@"
STUBEOF
chmod +x "$STUB"

# Three REAL plugin names from marketplace.json.
PLUGIN_A="blastguard"; PLUGIN_B="backlog"; PLUGIN_C="overwatch"

seed_sandbox() {
  TEST_CACHE="$TMP/cache/yukineko"; TEST_REGISTRY="$TMP/installed_plugins.json"; TEST_HOME="$TMP/home"
  rm -rf "$TEST_CACHE" "$TEST_HOME" "$TEST_REGISTRY"
  rm -f "$COUNTER"
  mkdir -p "$TEST_CACHE" "$TEST_HOME"
  PRIOR_A="$TEST_CACHE/$PLUGIN_A/0.0.1-prior"; PRIOR_B="$TEST_CACHE/$PLUGIN_B/0.0.2-prior"; PRIOR_C="$TEST_CACHE/$PLUGIN_C/0.0.3-prior"
  mkdir -p "$PRIOR_A" "$PRIOR_B" "$PRIOR_C"
  cat >"$TEST_REGISTRY" <<JSON
{ "version": 1, "plugins": {
    "${PLUGIN_A}@yukineko": [ {"scope":"user","installPath":"${PRIOR_A}","version":"0.0.1-prior"} ],
    "${PLUGIN_B}@yukineko": [ {"scope":"user","installPath":"${PRIOR_B}","version":"0.0.2-prior"} ],
    "${PLUGIN_C}@yukineko": [ {"scope":"user","installPath":"${PRIOR_C}","version":"0.0.3-prior"} ]
} }
JSON
}

reg_field() { # <key> <field>
  REG_KEY="$1" REG_FIELD="$2" TEST_REGISTRY_PATH="$TEST_REGISTRY" python3 -c '
import json, os
d = json.load(open(os.environ["TEST_REGISTRY_PATH"]))
print(d["plugins"][os.environ["REG_KEY"]][0][os.environ["REG_FIELD"]])'
}

gate_calls() { # how many times the stub gate was invoked
  if [ -f "$COUNTER" ]; then cat "$COUNTER"; else echo 0; fi
}

# =============================================================================
# Case A: SINGLE STAGE, gate advises ROLLBACK (rc=3).
# Before the fix the gate was never invoked at all for a single-stage canary and
# the run completed successfully. Expected now: gate runs once, the only stage
# is rolled back, exit 4.
# =============================================================================
echo
echo ">>> case A: single-stage canary, gate rc=3 — expect gate to RUN, rollback, exit 4"
seed_sandbox
OUT_A="$(
  set +e
  GATE_FAIL_FROM_CALL=1 \
  OVERWATCH_BIN="$STUB" CLAUDE_PLUGIN_CACHE="$TEST_CACHE" CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
  HOME="$TEST_HOME" OVERWATCH_CANARY_SINCE=1 \
  bash "$SCRIPT" --plugin "$PLUGIN_A" \
    --canary --canary-stage-size 1 --canary-threshold 0 --no-rebuild --no-sync 2>&1
  echo "RC=$?"
)"
RC_A="$(grep -o 'RC=[0-9]*$' <<<"$OUT_A" | tail -1 | cut -d= -f2)"
CALLS_A="$(gate_calls)"
echo "$OUT_A" | sed 's/^/    /'
echo "(exit code: $RC_A, gate invocations: $CALLS_A)"

grep -q "canary: 1 stage(s)" <<<"$OUT_A" || fail "case A did not produce a single-stage plan"
[ "$CALLS_A" -eq 1 ] || fail "case A: expected the health gate to be invoked exactly 1 time, got $CALLS_A (the only stage was never gated)"
pass "the only stage WAS health-gated (1 gate invocation)"
grep -q "health-gate: checking violation rate" <<<"$OUT_A" || fail "case A: no health-gate announcement printed"
pass "health-gate announcement printed for the only stage"
[ "$RC_A" -eq 4 ] || fail "case A: expected exit 4 (rollback + halt on rc=3), got $RC_A"
pass "single-stage rc=3 exited 4 (rollback + halt)"
grep -q "health-gate: ROLLBACK" <<<"$OUT_A" || fail "case A: did not report ROLLBACK"
pass "reported ROLLBACK"
[ "$(reg_field "${PLUGIN_A}@yukineko" version)" = "0.0.1-prior" ] \
  || fail "case A: $PLUGIN_A was NOT rolled back to its prior version"
pass "the only stage was rolled back to prior (registry restored)"
grep -q "rebuild: skipped (--no-rebuild)" <<<"$OUT_A" \
  && fail "case A: reached run_rebuild_and_sync after a ROLLBACK halt"
pass "did NOT reach run_rebuild_and_sync after the rollback halt"

# =============================================================================
# Case B: TWO STAGES, gate healthy for stage 0 and rc=3 for the FINAL stage.
# Before the fix the final stage was never gated, so the run completed
# successfully. Expected now: 2 gate invocations, the final stage rolled back,
# exit 4, and the already-passed stage 0 left applied (rollback is per-stage).
# =============================================================================
echo
echo ">>> case B: 2-stage canary, gate healthy for stage 0 and rc=3 for the FINAL stage — expect rollback, exit 4"
seed_sandbox
OUT_B="$(
  set +e
  GATE_FAIL_FROM_CALL=2 \
  OVERWATCH_BIN="$STUB" CLAUDE_PLUGIN_CACHE="$TEST_CACHE" CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
  HOME="$TEST_HOME" OVERWATCH_CANARY_SINCE=1 \
  bash "$SCRIPT" --plugin "$PLUGIN_A" --plugin "$PLUGIN_B" --plugin "$PLUGIN_C" \
    --canary --canary-stage-size 2 --canary-threshold 0 --no-rebuild --no-sync 2>&1
  echo "RC=$?"
)"
RC_B="$(grep -o 'RC=[0-9]*$' <<<"$OUT_B" | tail -1 | cut -d= -f2)"
CALLS_B="$(gate_calls)"
echo "$OUT_B" | sed 's/^/    /'
echo "(exit code: $RC_B, gate invocations: $CALLS_B)"

grep -q "canary: 2 stage(s)" <<<"$OUT_B" || fail "case B did not produce a two-stage plan"
[ "$CALLS_B" -eq 2 ] || fail "case B: expected 2 gate invocations (one per stage), got $CALLS_B (the final stage was never gated)"
pass "every stage — including the FINAL one — was health-gated (2 gate invocations)"
grep -q "health-gate: PROCEED" <<<"$OUT_B" || fail "case B: stage 0's healthy gate did not PROCEED"
pass "stage 0's healthy gate PROCEEDed"
[ "$RC_B" -eq 4 ] || fail "case B: expected exit 4 (final-stage rollback + halt), got $RC_B"
pass "final-stage rc=3 exited 4 (rollback + halt)"
grep -q "HALTED at stage 1 after auto-rollback" <<<"$OUT_B" || fail "case B: did not halt at the FINAL stage (1)"
pass "halted at the final stage (stage 1)"
[ "$(reg_field "${PLUGIN_C}@yukineko" version)" = "0.0.3-prior" ] \
  || fail "case B: final-stage plugin $PLUGIN_C was NOT rolled back to prior"
pass "the final stage was rolled back to prior"
[ "$(reg_field "${PLUGIN_A}@yukineko" version)" = "0.0.1-prior" ] \
  && fail "case B: stage 0 (which passed its gate) was also rolled back — rollback must be per-stage"
pass "stage 0 (gate-passed) left applied — rollback stayed scoped to the failing stage"
grep -q "rebuild: skipped (--no-rebuild)" <<<"$OUT_B" \
  && fail "case B: reached run_rebuild_and_sync after a ROLLBACK halt"
pass "did NOT reach run_rebuild_and_sync after the final-stage rollback halt"

# =============================================================================
# Case C: ANTI-VACUITY — single stage with a HEALTHY gate (rc=0) must still
# succeed and still reach run_rebuild_and_sync. An implementation that "fixes"
# the gap by always halting fails here.
# =============================================================================
echo
echo ">>> case C (anti-vacuity): single-stage canary, HEALTHY gate (rc=0) — expect PROCEED, exit 0, rebuild reached"
seed_sandbox
OUT_C="$(
  set +e
  GATE_FAIL_FROM_CALL=0 \
  OVERWATCH_BIN="$STUB" CLAUDE_PLUGIN_CACHE="$TEST_CACHE" CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
  HOME="$TEST_HOME" OVERWATCH_CANARY_SINCE=1 \
  bash "$SCRIPT" --plugin "$PLUGIN_A" \
    --canary --canary-stage-size 1 --canary-threshold 0 --no-rebuild --no-sync 2>&1
  echo "RC=$?"
)"
RC_C="$(grep -o 'RC=[0-9]*$' <<<"$OUT_C" | tail -1 | cut -d= -f2)"
CALLS_C="$(gate_calls)"
echo "$OUT_C" | sed 's/^/    /'
echo "(exit code: $RC_C, gate invocations: $CALLS_C)"

[ "$CALLS_C" -eq 1 ] || fail "case C: expected exactly 1 gate invocation, got $CALLS_C"
pass "the only stage was health-gated (1 gate invocation)"
grep -q "health-gate: PROCEED" <<<"$OUT_C" || fail "case C: healthy gate did not report PROCEED"
pass "healthy gate reported PROCEED"
[ "$RC_C" -eq 0 ] || fail "case C: a healthy single-stage canary must exit 0, got $RC_C (does the fix always halt?)"
pass "healthy single-stage canary exited 0"
grep -q "health-gate: ROLLBACK" <<<"$OUT_C" && fail "case C: rolled back a HEALTHY stage"
pass "no rollback on a healthy stage"
grep -q "rebuild: skipped (--no-rebuild)" <<<"$OUT_C" \
  || fail "case C: run_rebuild_and_sync was NOT reached on a healthy single-stage canary"
pass "run_rebuild_and_sync was reached (rollout completed)"
[ "$(reg_field "${PLUGIN_A}@yukineko" version)" = "0.0.1-prior" ] \
  && fail "case C: a healthy stage was rolled back to prior"
pass "the healthy stage stayed applied"

echo
echo "PASS: the canary health gate runs for EVERY stage including the last (or"
echo "      only) one; rc=3 rolls that stage back and halts with exit 4, while a"
echo "      healthy single-stage canary still proceeds to run_rebuild_and_sync."
