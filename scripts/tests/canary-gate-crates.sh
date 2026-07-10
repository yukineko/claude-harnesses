#!/usr/bin/env bash
# Self-contained test for Problem-2.3: `scripts/rollout-plugins.sh` MUST refuse
# to roll out a GATE crate (prompt-injection / spec / mutation defenses) without
# a canary, must proceed WITH --canary, must allow --no-canary as an explicit
# escape hatch, and must leave NON-gate crates unaffected (canary optional).
#
# Everything runs with CLAUDE_PLUGIN_CACHE / CLAUDE_PLUGIN_REGISTRY pointed at
# TEMP dirs and always with --dry-run for the "proceeds" cases, so no real
# rollout ever happens and neither the real cache/registry nor the temp files
# are mutated. Cleaned up on exit.
#
# Exit 0 on success, non-zero on any failed assertion.
set -uo pipefail

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

# --- temp cache + registry (nothing real is ever touched) --------------------
TMP="$(mktemp -d "${TMPDIR:-/tmp}/canary-gate-crates.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

TEST_CACHE="$TMP/cache/yukineko"
TEST_REGISTRY="$TMP/installed_plugins.json"
mkdir -p "$TEST_CACHE"

cat >"$TEST_REGISTRY" <<'JSON'
{
  "version": 1,
  "plugins": {
    "specguard@yukineko": [
      {"scope":"user","installPath":"/nonexistent/specguard/0.0.0","version":"0.0.0"}
    ],
    "overwatch@yukineko": [
      {"scope":"user","installPath":"/nonexistent/overwatch/0.0.0","version":"0.0.0"}
    ]
  }
}
JSON

REG_BEFORE_SUM="$(sha256sum "$TEST_REGISTRY" | awk '{print $1}')"

# Helper: run the rollout script with the temp cache/registry and given args.
# Captures combined stdout+stderr into $OUT and the exit code into $RC.
run_rollout() {
  OUT="$(
    OVERWATCH_BIN="$OW" \
    CLAUDE_PLUGIN_CACHE="$TEST_CACHE" \
    CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
    bash "$SCRIPT" "$@" 2>&1
  )"
  RC=$?
}

echo
echo "=== case 1: gate crate WITHOUT canary must ERROR (non-zero) ==="
run_rollout --plugin specguard
echo "$OUT" | sed 's/^/    /'
echo "(exit code: $RC)"
[ "$RC" -ne 0 ] || fail "gate crate specguard without --canary should have exited non-zero"
grep -qi "refusing to roll out gate crate" <<<"$OUT" \
  || fail "expected a clear gate-crate refusal message"
grep -q "specguard" <<<"$OUT" || fail "refusal should name the offending gate crate"
pass "specguard without canary is rejected with a clear error"

echo
echo "=== case 2: gate crate WITH --canary must proceed (dry-run) ==="
run_rollout --plugin specguard --canary --dry-run
echo "$OUT" | sed 's/^/    /'
echo "(exit code: $RC)"
[ "$RC" -eq 0 ] || fail "specguard --canary --dry-run should have exited 0 (got $RC)"
grep -q "canary stage plan" <<<"$OUT" || fail "expected the staged plan under --canary"
pass "specguard with --canary proceeds (staged, dry-run)"

echo
echo "=== case 3: gate crate WITH --no-canary override must proceed (dry-run) ==="
run_rollout --plugin specguard --no-canary --dry-run
echo "$OUT" | sed 's/^/    /'
echo "(exit code: $RC)"
[ "$RC" -eq 0 ] || fail "specguard --no-canary --dry-run should have exited 0 (got $RC)"
grep -qi "refusing to roll out gate crate" <<<"$OUT" \
  && fail "--no-canary should suppress the gate-crate refusal"
pass "specguard with --no-canary override proceeds (escape hatch)"

echo
echo "=== case 4: NON-gate crate WITHOUT canary still works (dry-run) ==="
run_rollout --plugin overwatch --dry-run
echo "$OUT" | sed 's/^/    /'
echo "(exit code: $RC)"
[ "$RC" -eq 0 ] || fail "non-gate crate overwatch without canary should exit 0 (got $RC)"
grep -qi "refusing to roll out gate crate" <<<"$OUT" \
  && fail "non-gate crate must NOT trigger the gate-crate refusal"
pass "non-gate crate (overwatch) without canary is unaffected"

# --- nothing was mutated (all cases used --dry-run or errored pre-mutation) ---
REG_AFTER_SUM="$(sha256sum "$TEST_REGISTRY" | awk '{print $1}')"
[ "$REG_BEFORE_SUM" = "$REG_AFTER_SUM" ] || fail "temp registry changed during the test"
pass "temp registry unchanged"
CACHE_ENTRIES="$(find "$TEST_CACHE" -mindepth 1 2>/dev/null | wc -l | tr -d ' ')"
[ "$CACHE_ENTRIES" -eq 0 ] || fail "temp cache had $CACHE_ENTRIES entries created"
pass "temp cache untouched"

echo
echo "PASS: gate crates require --canary; --no-canary overrides; non-gate crates unaffected."
