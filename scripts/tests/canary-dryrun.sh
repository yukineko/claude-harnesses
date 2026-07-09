#!/usr/bin/env bash
# Self-contained test for the opt-in canary staged rollout in
# scripts/rollout-plugins.sh.
#
# It runs `rollout-plugins.sh --canary --dry-run` with CLAUDE_PLUGIN_CACHE and
# CLAUDE_PLUGIN_REGISTRY pointed at TEMP dirs, and asserts:
#   (a) it prints the staged plan + the rollback plan, and
#   (b) it mutates NOTHING — neither the temp registry file NOR the real
#       ~/.claude/plugins tree is touched (verified by mtime/content snapshot).
#
# HARD SAFETY: this test NEVER runs a real (non-dry-run) rollout and NEVER
# points at the real cache/registry. Everything is under a mktemp -d that is
# cleaned up on exit.
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

# --- temp cache + registry (nothing real is ever touched) --------------------
TMP="$(mktemp -d "${TMPDIR:-/tmp}/canary-dryrun.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

TEST_CACHE="$TMP/cache/yukineko"
TEST_REGISTRY="$TMP/installed_plugins.json"
mkdir -p "$TEST_CACHE"

# Seed a minimal but valid registry with a couple of known plugins so the
# canary path has prior-state to reason about. Versions here are intentionally
# arbitrary/old — the dry run must not care.
cat >"$TEST_REGISTRY" <<'JSON'
{
  "version": 1,
  "plugins": {
    "overwatch@yukineko": [
      {"scope":"user","installPath":"/nonexistent/overwatch/0.0.0","version":"0.0.0"}
    ],
    "condukt@yukineko": [
      {"scope":"user","installPath":"/nonexistent/condukt/0.0.0","version":"0.0.0"}
    ]
  }
}
JSON

# Snapshot the temp registry so we can prove the dry run did not mutate it.
REG_BEFORE_SUM="$(sha256sum "$TEST_REGISTRY" | awk '{print $1}')"
REG_BEFORE_MTIME="$(stat -c %Y "$TEST_REGISTRY")"

# --- snapshot the REAL ~/.claude/plugins to prove it's never touched ---------
REAL_PLUGINS="$HOME/.claude/plugins"
REAL_BEFORE=""
if [ -d "$REAL_PLUGINS" ]; then
  # Hash the full listing (names + sizes + mtimes) of the real tree.
  REAL_BEFORE="$(find "$REAL_PLUGINS" -printf '%p|%s|%T@\n' 2>/dev/null | sort | sha256sum | awk '{print $1}')"
fi

# --- run the dry-run canary rollout ------------------------------------------
echo
echo ">>> running: rollout-plugins.sh --canary --dry-run (temp cache/registry)"
OUT="$(
  OVERWATCH_BIN="$OW" \
  CLAUDE_PLUGIN_CACHE="$TEST_CACHE" \
  CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
  bash "$SCRIPT" --canary --canary-stage-size 2 --dry-run 2>&1
)"
RC=$?
echo "$OUT" | sed 's/^/    /'
echo "(exit code: $RC)"
[ "$RC" -eq 0 ] || fail "dry-run canary rollout exited non-zero ($RC)"

# --- assertions (a): staged plan + rollback plan printed ---------------------
echo
echo "assertions:"
grep -q "canary stage plan" <<<"$OUT" || fail "did not print the staged plan header"
pass "printed the staged plan"
grep -q "canary rollback plan" <<<"$OUT" || fail "did not print the rollback plan header"
pass "printed the rollback plan"
grep -q '"stages"' <<<"$OUT" || fail "staged plan JSON missing 'stages'"
pass "staged plan JSON contains stages"
grep -q '"targets"' <<<"$OUT" || fail "rollback plan JSON missing 'targets'"
pass "rollback plan JSON contains targets"
grep -Eq '\[dry-run\] would copy' <<<"$OUT" || fail "dry-run did not report would-copy actions"
pass "dry-run reported would-copy actions (no real copy)"
grep -q "done (canary)" <<<"$OUT" || fail "canary path did not complete"
pass "canary path completed"

# --- assertion: a SUCCESSFUL canary reaches rebuild + sync (finding 4) --------
# The canary path used to copy + repoint the registry and then exit 0 without
# ever running rebuild-plugins.sh / sync-plugin-assets.sh, silently leaving the
# harness on stale binaries. A full-stage (no rollback) dry-run must now show
# it reaches both stages. Under --dry-run these only PRINT (nothing is really
# rebuilt/synced — the mutation guards below still hold).
grep -q "would run: scripts/rebuild-plugins.sh" <<<"$OUT" \
  || fail "successful canary did not reach the rebuild stage (finding 4 regression)"
pass "canary reaches the rebuild stage (finding 4)"
grep -Eq "would run:.*sync-plugin-assets.sh|sync: no plugin ships" <<<"$OUT" \
  || fail "successful canary did not reach the asset-sync stage (finding 4 regression)"
pass "canary reaches the asset-sync stage (finding 4)"

# --- assertions (b): NOTHING mutated -----------------------------------------
REG_AFTER_SUM="$(sha256sum "$TEST_REGISTRY" | awk '{print $1}')"
REG_AFTER_MTIME="$(stat -c %Y "$TEST_REGISTRY")"
[ "$REG_BEFORE_SUM" = "$REG_AFTER_SUM" ] || fail "temp registry CONTENT changed during --dry-run"
[ "$REG_BEFORE_MTIME" = "$REG_AFTER_MTIME" ] || fail "temp registry was rewritten (mtime changed) during --dry-run"
pass "temp registry unchanged (content + mtime)"

# No version dirs should have been created under the temp cache.
CACHE_ENTRIES="$(find "$TEST_CACHE" -mindepth 1 2>/dev/null | wc -l | tr -d ' ')"
[ "$CACHE_ENTRIES" -eq 0 ] || fail "temp cache had $CACHE_ENTRIES entries created during --dry-run"
pass "temp cache untouched (no version dirs created)"

# No .bak-* backups should have been created next to the temp registry.
BAKS="$(find "$(dirname "$TEST_REGISTRY")" -name 'installed_plugins.json.bak-*' 2>/dev/null | wc -l | tr -d ' ')"
[ "$BAKS" -eq 0 ] || fail "registry backup files were created during --dry-run"
pass "no registry backups created"

# The REAL ~/.claude/plugins tree must be byte-for-byte identical.
if [ -n "$REAL_BEFORE" ]; then
  REAL_AFTER="$(find "$REAL_PLUGINS" -printf '%p|%s|%T@\n' 2>/dev/null | sort | sha256sum | awk '{print $1}')"
  [ "$REAL_BEFORE" = "$REAL_AFTER" ] || fail "REAL ~/.claude/plugins tree changed during --dry-run"
  pass "REAL ~/.claude/plugins untouched"
else
  pass "REAL ~/.claude/plugins does not exist here — nothing to guard"
fi

echo
echo "PASS: canary dry-run prints staged + rollback plans and mutates nothing."
