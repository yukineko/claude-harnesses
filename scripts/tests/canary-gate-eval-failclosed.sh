#!/usr/bin/env bash
# Self-contained test for the FAIL-CLOSED handling of a canary health-gate that
# CANNOT EVALUATE (c8a962dd). Historically rollout-plugins.sh treated any
# canary-gate exit code other than 0 (proceed) / 3 (rollback) as a benign
# "eval error" and PROCEEDed (fail-soft) — so a GATE-crate rollout advanced
# every stage precisely because the health check was broken and verified
# nothing. This test proves the new default: an un-evaluable gate ROLLS THE
# STAGE BACK and HALTS (exit 5), and the explicit ROLLOUT_GATE_EVAL_FAILSOFT=1
# escape restores the old PROCEED for the documented overwatch bootstrap-skew.
#
# It also (re)exercises the extracted execute_stage_rollback() helper via the
# eval-error path's emit_record=0 mode, and asserts NO violation-rollback event
# is written (there was no health verdict to attribute).
#
# MAC-RUNNABLE: unlike canary-rollback.sh, this test does NOT snapshot the real
# ~/.claude/plugins tree with GNU `find -printf` / `sha256sum` (which exit under
# `set -e` on BSD/mac). Everything runs against a TEMP cache/registry/HOME under
# mktemp -d, cleaned up on exit; the real tree is never pointed at or touched.
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

TMP="$(mktemp -d "${TMPDIR:-/tmp}/canary-gate-eval.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# --- stub overwatch: exit 2 (simulated clap usage error) for canary-gate ONLY,
# delegate every other subcommand (canary-rollback-plan / record-*) to the real
# binary. This reproduces the bootstrap-skew "gate flag rejected by the pre-swap
# binary" case without needing an actually-old binary.
STUB="$TMP/ow-stub.sh"
cat >"$STUB" <<STUBEOF
#!/usr/bin/env bash
if [ "\$1" = "canary-gate" ]; then
  echo "error: unexpected argument '--simulated' found (stub clap usage error)" >&2
  exit 2
fi
exec "$OW" "\$@"
STUBEOF
chmod +x "$STUB"

# Three REAL plugin names from marketplace.json; stage-size 2 => stage0=[A,B],
# stage1=[C]. The single inter-stage gate check (after stage 0) is where the
# eval error is injected. C (stage 1) must NEVER be reached.
PLUGIN_A="blastguard"; PLUGIN_B="backlog"; PLUGIN_C="overwatch"

seed_sandbox() {
  # (re)build a fresh temp cache + registry pointing all three at PRIOR dirs.
  TEST_CACHE="$TMP/cache/yukineko"; TEST_REGISTRY="$TMP/installed_plugins.json"; TEST_HOME="$TMP/home"
  rm -rf "$TEST_CACHE" "$TEST_HOME" "$TEST_REGISTRY"
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

# =============================================================================
# Part 1: DEFAULT — an un-evaluable gate FAILS CLOSED (rollback + halt, exit 5).
# =============================================================================
echo
echo ">>> default: canary-gate exits 2 (cannot evaluate) — expect fail-closed halt (exit 5)"
seed_sandbox
OUT="$(
  set +e
  OVERWATCH_BIN="$STUB" CLAUDE_PLUGIN_CACHE="$TEST_CACHE" CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
  HOME="$TEST_HOME" OVERWATCH_CANARY_SINCE=1 \
  bash "$SCRIPT" --plugin "$PLUGIN_A" --plugin "$PLUGIN_B" --plugin "$PLUGIN_C" \
    --canary --canary-stage-size 2 --canary-threshold 0 --no-rebuild --no-sync 2>&1
  echo "RC=$?"
)"
RC="$(grep -o 'RC=[0-9]*$' <<<"$OUT" | tail -1 | cut -d= -f2)"
echo "$OUT" | sed 's/^/    /'
echo "(exit code: $RC)"

[ "$RC" -eq 5 ] || fail "expected exit 5 (fail-closed halt on un-evaluable gate), got $RC"
pass "script exited 5 (fail-closed halt)"
grep -q "health-gate: CANNOT EVALUATE (rc=2)" <<<"$OUT" || fail "did not report CANNOT EVALUATE"
pass "reported CANNOT EVALUATE"
grep -q "could not evaluate (fail-closed)" <<<"$OUT" || fail "did not report the fail-closed halt"
pass "reported fail-closed halt"
grep -q "treating as no-spike and PROCEEDING" <<<"$OUT" && fail "STILL fail-soft PROCEEDs — the fail-open is not closed"
pass "did NOT fail-soft proceed (fail-open closed)"

# stage-0 plugins were applied then ROLLED BACK to prior; C (stage 1) untouched.
grep -q "copied $PLUGIN_A ->" <<<"$OUT" || fail "$PLUGIN_A never applied before rollback"
grep -q "copied $PLUGIN_C ->" <<<"$OUT" && fail "$PLUGIN_C (stage 1) was applied — halt should precede stage 1"
pass "$PLUGIN_C (stage 1) correctly never reached"
[ "$(reg_field "${PLUGIN_A}@yukineko" version)" = "0.0.1-prior" ] || fail "$PLUGIN_A not rolled back to prior"
[ "$(reg_field "${PLUGIN_B}@yukineko" version)" = "0.0.2-prior" ] || fail "$PLUGIN_B not rolled back to prior"
pass "stage-0 plugins rolled back to prior version (registry restored)"

# emit_record=0: NO violation-rollback event should have been written.
ROLLBACK_EVENTS="$(find "$TEST_HOME" -name 'rollbacks.jsonl' -size +0c 2>/dev/null | wc -l | tr -d ' ')"
[ "$ROLLBACK_EVENTS" = "0" ] || fail "a violation-rollback event was recorded for an eval-error (should be emit_record=0)"
pass "no false violation-rollback event recorded (emit_record=0 honored)"

# =============================================================================
# Part 2: ROLLOUT_GATE_EVAL_FAILSOFT=1 — explicit operator override PROCEEDs.
# =============================================================================
echo
echo ">>> ROLLOUT_GATE_EVAL_FAILSOFT=1: same eval error, explicit override — expect PROCEED to completion"
seed_sandbox
OUT2="$(
  set +e
  ROLLOUT_GATE_EVAL_FAILSOFT=1 \
  OVERWATCH_BIN="$STUB" CLAUDE_PLUGIN_CACHE="$TEST_CACHE" CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
  HOME="$TEST_HOME" OVERWATCH_CANARY_SINCE=1 \
  bash "$SCRIPT" --plugin "$PLUGIN_A" --plugin "$PLUGIN_B" --plugin "$PLUGIN_C" \
    --canary --canary-stage-size 2 --canary-threshold 0 --no-rebuild --no-sync 2>&1
  echo "RC=$?"
)"
RC2="$(grep -o 'RC=[0-9]*$' <<<"$OUT2" | tail -1 | cut -d= -f2)"
echo "$OUT2" | sed 's/^/    /'
echo "(exit code: $RC2)"

[ "$RC2" -eq 0 ] || fail "expected exit 0 (explicit override proceeds to completion), got $RC2"
pass "explicit override exited 0 (proceeded)"
grep -q "explicit operator override to PROCEED" <<<"$OUT2" || fail "did not log the explicit override"
pass "logged the explicit operator override"
grep -q "copied $PLUGIN_C ->" <<<"$OUT2" || fail "$PLUGIN_C (stage 1) was NOT applied under override — should have proceeded"
pass "$PLUGIN_C (stage 1) applied under override (rollout proceeded past the gate)"

# =============================================================================
# Part 3: a REAL rollback verdict (gate exit 3) still rolls back + halts exit 4
# AND records the violation-rollback event — proving the extracted
# execute_stage_rollback() helper's emit_record=1 branch is intact after the
# refactor (canary-rollback.sh covers this end-to-end too, but only on CI /
# GNU-coreutils; this gives the same coverage mac-locally via a stub).
# =============================================================================
echo
echo ">>> gate exits 3 (real rollback verdict) — expect rollback + halt (exit 4) + recorded event"
STUB3="$TMP/ow-stub3.sh"
cat >"$STUB3" <<STUBEOF
#!/usr/bin/env bash
if [ "\$1" = "canary-gate" ]; then exit 3; fi
exec "$OW" "\$@"
STUBEOF
chmod +x "$STUB3"
seed_sandbox
OUT3="$(
  set +e
  OVERWATCH_BIN="$STUB3" CLAUDE_PLUGIN_CACHE="$TEST_CACHE" CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
  HOME="$TEST_HOME" OVERWATCH_CANARY_SINCE=1 \
  bash "$SCRIPT" --plugin "$PLUGIN_A" --plugin "$PLUGIN_B" --plugin "$PLUGIN_C" \
    --canary --canary-stage-size 2 --canary-threshold 0 --no-rebuild --no-sync 2>&1
  echo "RC=$?"
)"
RC3="$(grep -o 'RC=[0-9]*$' <<<"$OUT3" | tail -1 | cut -d= -f2)"
echo "$OUT3" | sed 's/^/    /'
echo "(exit code: $RC3)"
[ "$RC3" -eq 4 ] || fail "expected exit 4 (rollback + halt on rc=3), got $RC3"
pass "rc=3 verdict exited 4 (rollback + halt) via the refactored helper"
grep -q "health-gate: ROLLBACK" <<<"$OUT3" || fail "did not report ROLLBACK for rc=3"
pass "reported ROLLBACK"
[ "$(reg_field "${PLUGIN_A}@yukineko" version)" = "0.0.1-prior" ] || fail "$PLUGIN_A not rolled back on rc=3"
pass "stage-0 rolled back to prior on rc=3"
RB3="$(find "$TEST_HOME" -name 'rollbacks.jsonl' -size +0c 2>/dev/null | wc -l | tr -d ' ')"
[ "$RB3" = "1" ] || fail "expected a recorded violation-rollback event for rc=3 (emit_record=1), found $RB3"
pass "violation-rollback event recorded (emit_record=1 honored)"

echo
echo "PASS: an un-evaluable canary gate fails CLOSED (rollback + halt, exit 5) by"
echo "      default, records no false rollback event, and only PROCEEDs under the"
echo "      explicit ROLLOUT_GATE_EVAL_FAILSOFT=1 acknowledgement; a real rc=3"
echo "      verdict still rolls back + halts (exit 4) and records its event."
