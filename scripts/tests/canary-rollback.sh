#!/usr/bin/env bash
# Self-contained test for the auto-rollback EXECUTION path of the opt-in
# canary staged rollout in scripts/rollout-plugins.sh.
#
# canary-dryrun.sh only exercises --dry-run (planning); it never runs the
# real rollback branch (the one that re-points installed_plugins.json back to
# the PRIOR version dir after a health-gate ROLLBACK verdict). This test
# actually EXECUTES that branch — entirely inside a temp sandbox — and
# asserts the registry is correctly restored to the version/path that was
# live before the canary moved it.
#
# It also proves the quote-injection fix at the unit level: the plugin-name
# lookup used by the rollback branch (rollback_target_lookup() in
# rollout-plugins.sh) used to splice the plugin name directly into a Python
# string literal (`t["name"]=="$pn"`), which a name containing a quote could
# break out of / inject into. `--plugin` (and thus the canary plugin set) is
# validated against this repo's real marketplace.json, so a fake quote-named
# *plugin* can't be driven through the end-to-end CLI without editing crate
# files (out of scope for a scripts-only change) — instead we call the fixed
# helper function directly with an adversarial name and prove (a) correct
# lookup for a quoted name and (b) no code execution from an adversarial one.
# The end-to-end run below then proves the SAME helper is what the real
# rollback branch calls, using real plugin names.
#
# HARD SAFETY: everything runs against TEMP CLAUDE_PLUGIN_CACHE /
# CLAUDE_PLUGIN_REGISTRY / HOME (for the overwatch violation store) under
# mktemp -d, cleaned up on exit. The REAL ~/.claude/plugins tree is
# snapshotted before/after and asserted byte-for-byte unchanged, exactly like
# canary-dryrun.sh. This test NEVER points rollout-plugins.sh's cache or
# registry at the real ~/.claude/plugins, and NEVER edits marketplace.json /
# any plugin.json / any crate.
#
# Exit 0 on success, non-zero on any failed assertion.
set -euo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$REPO/scripts/rollout-plugins.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

# --- locate / build the overwatch binary (deterministic canary core) ---------
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

# --- temp cache + registry + HOME (nothing real is ever touched) -------------
TMP="$(mktemp -d "${TMPDIR:-/tmp}/canary-rollback.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

TEST_CACHE="$TMP/cache/yukineko"
TEST_REGISTRY="$TMP/installed_plugins.json"
TEST_HOME="$TMP/home"
mkdir -p "$TEST_CACHE" "$TEST_HOME"

# Three REAL plugin names from this repo's marketplace.json
# (rollout-plugins.sh validates --plugin against it, so synthetic names can't
# be driven through the end-to-end CLI without editing crate files — out of
# scope here). PLUGIN_A and PLUGIN_B share stage 0 (--canary-stage-size 2, 3
# plugins -> stage 0 = [A,B], stage 1 = [C]) so BOTH get genuinely applied
# (mutated) and then rolled back together when the health gate trips between
# stage 0 and stage 1 — proving a real mutate-then-restore for both, not just
# "never touched, still equals prior". PLUGIN_C is filler that must NEVER be
# reached (the halt happens before stage 1).
#
# NOTE: stage order follows marketplace.json's plugin listing order (not
# --plugin CLI arg order or alphabetical order) — A and B are chosen so they
# fall in marketplace.json before C, which puts them together in stage 0.
PLUGIN_A="blastguard"
PLUGIN_B="backlog"
PLUGIN_C="overwatch"

# Prior (pre-canary) install dirs: these must already exist in the cache —
# rollback only repoints the registry, it does not recreate old version dirs
# (see rollout-plugins.sh's "never deletes old version dirs" invariant).
PRIOR_A_VER="0.0.1-prior"
PRIOR_B_VER="0.0.2-prior"
PRIOR_C_VER="0.0.3-prior"
PRIOR_A_DIR="$TEST_CACHE/$PLUGIN_A/$PRIOR_A_VER"
PRIOR_B_DIR="$TEST_CACHE/$PLUGIN_B/$PRIOR_B_VER"
PRIOR_C_DIR="$TEST_CACHE/$PLUGIN_C/$PRIOR_C_VER"
mkdir -p "$PRIOR_A_DIR" "$PRIOR_B_DIR" "$PRIOR_C_DIR"
echo "prior-a-marker" >"$PRIOR_A_DIR/MARKER"
echo "prior-b-marker" >"$PRIOR_B_DIR/MARKER"
echo "prior-c-marker" >"$PRIOR_C_DIR/MARKER"

# Seed the registry pointing all three plugins at their PRIOR version/path.
cat >"$TEST_REGISTRY" <<JSON
{
  "version": 1,
  "plugins": {
    "${PLUGIN_A}@yukineko": [
      {"scope":"user","installPath":"${PRIOR_A_DIR}","version":"${PRIOR_A_VER}"}
    ],
    "${PLUGIN_B}@yukineko": [
      {"scope":"user","installPath":"${PRIOR_B_DIR}","version":"${PRIOR_B_VER}"}
    ],
    "${PLUGIN_C}@yukineko": [
      {"scope":"user","installPath":"${PRIOR_C_DIR}","version":"${PRIOR_C_VER}"}
    ]
  }
}
JSON

# --- snapshot the REAL ~/.claude/plugins to prove it's never touched ---------
REAL_PLUGINS="$HOME/.claude/plugins"
REAL_BEFORE=""
if [ -d "$REAL_PLUGINS" ]; then
  REAL_BEFORE="$(find "$REAL_PLUGINS" -printf '%p|%s|%T@\n' 2>/dev/null | sort | sha256sum | awk '{print $1}')"
fi

# =============================================================================
# Part 1: exercise rollback_target_lookup() directly with a plugin name
# containing a literal double-quote, proving the injection fix at the
# smallest possible unit (argv-safe, not string-interpolated into Python).
# =============================================================================
echo
echo ">>> unit check: rollback_target_lookup() is argv-safe for a quoted name"
# Source only the helper function (avoids running the whole script under
# `set -e`) by extracting it with sed and eval'ing it in this shell.
eval "$(sed -n '/^rollback_target_lookup() {/,/^}/p' "$SCRIPT")"
[ -n "$(declare -f rollback_target_lookup 2>/dev/null || true)" ] || fail "could not extract rollback_target_lookup() from $SCRIPT"

QUOTE_NAME='fo"o'
QUOTE_DIR="$TMP/quote-target"
mkdir -p "$QUOTE_DIR"
RBPLAN_JSON='{"stage_index":0,"targets":[{"name":"fo\"o","is_new":false,"prior_version":"9.9.9-quote","restore_install_path":"'"$QUOTE_DIR"'"},{"name":"clean","is_new":true}]}'
LOOKUP_OUT="$(rollback_target_lookup "$RBPLAN_JSON" "$QUOTE_NAME")"
LOOKUP_VER="$(sed -n '1p' <<<"$LOOKUP_OUT")"
LOOKUP_PATH="$(sed -n '2p' <<<"$LOOKUP_OUT")"
[ "$LOOKUP_VER" = "9.9.9-quote" ] || fail "quote-name lookup returned wrong version: $LOOKUP_VER"
[ "$LOOKUP_PATH" = "$QUOTE_DIR" ] || fail "quote-name lookup returned wrong path: $LOOKUP_PATH"
pass "rollback_target_lookup() correctly resolves a plugin name containing a literal double-quote"

# is_new targets must resolve to empty (nothing to restore) — proves the
# lookup honors is_new and doesn't false-positive.
NEW_OUT="$(rollback_target_lookup "$RBPLAN_JSON" "clean")"
[ -z "$(tr -d '\n' <<<"$NEW_OUT")" ] || fail "is_new target should resolve to empty version+path, got: $NEW_OUT"
pass "is_new target correctly resolves to nothing-to-restore"

# A name that would have broken the OLD (string-interpolated) lookup by
# closing the Python string literal early and injecting code — must not
# match anything it isn't supposed to, and must not execute injected code.
INJECT_NAME='x"] ; import os,sys; sys.stderr.write("INJECTED\n") ; t=[{"name":"x'
INJECT_OUT="$(rollback_target_lookup "$RBPLAN_JSON" "$INJECT_NAME" 2>&1)" || fail "lookup with adversarial name crashed"
grep -q "INJECTED" <<<"$INJECT_OUT" && fail "adversarial plugin name executed injected Python code"
pass "adversarial quote-laden plugin name does not execute injected code"

# =============================================================================
# Part 2: end-to-end — drive the REAL (non-dry-run) canary path so it decides
# to roll back, and assert the registry is restored to the prior state, using
# real plugin names from this repo's marketplace.json.
# =============================================================================
echo
echo ">>> running: rollout-plugins.sh --canary (non-dry-run, temp sandbox) — expect auto-rollback"

# Seed a violation event via the real CLI (not hand-crafted JSONL) in the
# sandboxed overwatch store (HOME repointed) so canary-gate's registry
# path — the one the REAL (non-dry-run) run_canary branch actually calls —
# observes more violations than --canary-threshold 0 tolerates, deterministically
# tripping ROLLBACK. This exercises the exact code path production traffic
# would hit (no bypassing the gate with --observed-violations).
#
# IMPORTANT: rollout-plugins.sh does `cd "$(dirname "$0")/.."` at startup, so
# canary-gate's registry-mode read always resolves its project-key from
# $REPO's cwd (walking up to $REPO's .git), regardless of where this test
# script itself is invoked from. Record the violation from the SAME cwd
# (with HOME still repointed to the sandbox) so it lands in the store the
# script will actually read — otherwise the event would be recorded under an
# unrelated project-key and the gate would (correctly, but unhelpfully for
# this test) observe zero violations.
(
  cd "$REPO"
  HOME="$TEST_HOME" "$OW" record-violation --source blastguard --discriminator test-rule --task canary-rollback-test >/dev/null
)

# Pin the stage-deploy anchor (Problem-2.2 --since) to epoch 1 so the violation
# recorded above — whose wall-clock ts is far in the future relative to 1 —
# still falls at/after the anchor and is counted by the gate. Without this pin
# the auto-captured deploy timestamp would (correctly) be LATER than the
# just-recorded violation, excluding it as "pre-deploy"; pinning keeps this
# test exercising the ROLLBACK branch deterministically.
OUT="$(
  set +e
  OVERWATCH_BIN="$OW" \
  CLAUDE_PLUGIN_CACHE="$TEST_CACHE" \
  CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
  HOME="$TEST_HOME" \
  OVERWATCH_CANARY_SINCE=1 \
  bash "$SCRIPT" --plugin "$PLUGIN_A" --plugin "$PLUGIN_B" --plugin "$PLUGIN_C" \
    --canary --canary-stage-size 2 --canary-threshold 0 --no-rebuild --no-sync 2>&1
  echo "RC=$?"
)"
RC="$(grep -o 'RC=[0-9]*$' <<<"$OUT" | tail -1 | cut -d= -f2)"
echo "$OUT" | sed 's/^/    /'
echo "(exit code: $RC)"

# The script exits 4 specifically to signal "halted after auto-rollback" —
# assert that exact contract, not just non-zero.
[ "$RC" -eq 4 ] || fail "expected exit 4 (halted after auto-rollback), got $RC"
pass "script exited 4 (HALTED after auto-rollback)"

grep -q "health-gate: ROLLBACK" <<<"$OUT" || fail "did not report a ROLLBACK health-gate verdict"
pass "health-gate reported ROLLBACK"
grep -q "canary: HALTED at stage" <<<"$OUT" || fail "did not report HALTED after rollback"
pass "reported HALTED after rollback"

# Both A and B must have been GENUINELY applied (mutated to the canary
# version) before the rollback, not merely "left alone and still equal to
# prior" — proves this is a real mutate-then-restore round trip, not a
# vacuous no-op check.
grep -q "copied $PLUGIN_A ->" <<<"$OUT" || fail "$PLUGIN_A was never actually copied/applied before rollback"
grep -q "copied $PLUGIN_B ->" <<<"$OUT" || fail "$PLUGIN_B was never actually copied/applied before rollback"
pass "$PLUGIN_A and $PLUGIN_B were genuinely applied (mutated) before the rollback restored them"

# PLUGIN_C (stage 1) must NEVER have been reached — the halt happens between
# stage 0 and stage 1, before stage 1 applies anything.
grep -q "copied $PLUGIN_C ->" <<<"$OUT" && fail "$PLUGIN_C (stage 1) was applied — halt should have stopped before stage 1"
pass "$PLUGIN_C (stage 1) was correctly never reached"

# --- assertions: the registry was ACTUALLY restored to the prior state ------
# (env-passed keys, never interpolated into a Python literal, consistent
# with the fix under test.)
read_reg_field() {
  local key="$1" field="$2"
  REG_KEY="$key" REG_FIELD="$field" python3 -c '
import json, os
d = json.load(open(os.environ["TEST_REGISTRY_PATH"]))
print(d["plugins"][os.environ["REG_KEY"]][0][os.environ["REG_FIELD"]])
'
}
export TEST_REGISTRY_PATH="$TEST_REGISTRY"

REG_A_VER="$(read_reg_field "${PLUGIN_A}@yukineko" version)"
REG_A_PATH="$(read_reg_field "${PLUGIN_A}@yukineko" installPath)"
[ "$REG_A_VER" = "$PRIOR_A_VER" ] || fail "registry version for $PLUGIN_A not restored to prior ($PRIOR_A_VER); got $REG_A_VER"
[ "$REG_A_PATH" = "$PRIOR_A_DIR" ] || fail "registry installPath for $PLUGIN_A not restored to prior; got $REG_A_PATH"
pass "$PLUGIN_A registry entry restored to prior version/path after rollback"

REG_B_VER="$(read_reg_field "${PLUGIN_B}@yukineko" version)"
REG_B_PATH="$(read_reg_field "${PLUGIN_B}@yukineko" installPath)"
[ "$REG_B_VER" = "$PRIOR_B_VER" ] || fail "registry version for $PLUGIN_B not restored to prior ($PRIOR_B_VER); got $REG_B_VER"
[ "$REG_B_PATH" = "$PRIOR_B_DIR" ] || fail "registry installPath for $PLUGIN_B not restored to prior; got $REG_B_PATH"
pass "$PLUGIN_B registry entry restored to prior version/path after rollback"

REG_C_VER="$(read_reg_field "${PLUGIN_C}@yukineko" version)"
REG_C_PATH="$(read_reg_field "${PLUGIN_C}@yukineko" installPath)"
[ "$REG_C_VER" = "$PRIOR_C_VER" ] || fail "registry version for untouched $PLUGIN_C unexpectedly changed; got $REG_C_VER"
[ "$REG_C_PATH" = "$PRIOR_C_DIR" ] || fail "registry installPath for untouched $PLUGIN_C unexpectedly changed; got $REG_C_PATH"
pass "$PLUGIN_C (stage 1, never reached) registry entry is untouched"

# A backup of the registry must have been created (registry_patch always
# backs up before writing — proves the real write path ran, not a no-op).
BAKS="$(find "$TMP" -name 'installed_plugins.json.bak-*' 2>/dev/null | wc -l | tr -d ' ')"
[ "$BAKS" -ge 1 ] || fail "no registry backup was created — rollback write path may not have actually run"
pass "registry backup created (rollback write path genuinely executed, not a no-op)"

# The registry must still be valid JSON after the rollback write.
python3 -c "import json,sys; json.load(open(sys.argv[1]))" "$TEST_REGISTRY" || fail "registry is not valid JSON after rollback"
pass "registry is valid JSON after rollback"

# --- the REAL ~/.claude/plugins tree must be byte-for-byte identical --------
if [ -n "$REAL_BEFORE" ]; then
  REAL_AFTER="$(find "$REAL_PLUGINS" -printf '%p|%s|%T@\n' 2>/dev/null | sort | sha256sum | awk '{print $1}')"
  [ "$REAL_BEFORE" = "$REAL_AFTER" ] || fail "REAL ~/.claude/plugins tree changed during the sandboxed rollback test"
  pass "REAL ~/.claude/plugins untouched"
else
  pass "REAL ~/.claude/plugins does not exist here — nothing to guard"
fi

echo
echo "PASS: canary auto-rollback EXECUTION path (non-dry-run) correctly restores"
echo "      the prior version/path for every plugin, the quote-injection fix is"
echo "      proven at the unit level, and the real cache is never touched."
