#!/usr/bin/env bash
# Regression test for the GATE_CRATE canary-bypass gap: rebuild-plugins.sh used
# to swap EVERY installed plugin's binary into the live cache on every run
# (because `cargo build --workspace` rebuilds everything regardless of which
# single plugin a rollout targeted). That meant a plain, non-canaried
# `rollout-plugins.sh --plugin backlog` could silently deploy a fresh
# blastguard/overwatch/propguard/stuckguard binary as a side effect, bypassing
# the mandatory --canary health gate for those GATE crates entirely.
#
# The fix adds `rebuild-plugins.sh --only=<names>`, and rollout-plugins.sh's
# run_rebuild_and_sync() now always passes --only=<the exact plugin set THIS
# invocation targets>. This test proves both halves:
#   Part A: rollout-plugins.sh actually WIRES --only to the requested plugin
#           set (not the full fleet) when --plugin is given.
#   Part B: rebuild-plugins.sh's --only FILTER actually restricts which cache
#           binaries get refreshed (the enforcement point itself).
#
# Everything runs under --dry-run / temp cache+registry+target dirs. Nothing
# real is ever touched. Exit 0 on success, non-zero on any failed assertion.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
ROLLOUT="$REPO/scripts/rollout-plugins.sh"
REBUILD="$REPO/scripts/rebuild-plugins.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/rebuild-only-scoping.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# =============================================================================
# Part A: rollout-plugins.sh wires --only to the requested plugin set
# =============================================================================
echo "=== Part A: rollout-plugins.sh --plugin wires --only to that exact set ==="

TEST_CACHE="$TMP/cache/yukineko"
TEST_REGISTRY="$TMP/installed_plugins.json"
mkdir -p "$TEST_CACHE"
cat >"$TEST_REGISTRY" <<'JSON'
{
  "version": 1,
  "plugins": {
    "backlog@yukineko": [
      {"scope":"user","installPath":"/nonexistent/backlog/0.0.0","version":"0.0.0"}
    ],
    "session-insights@yukineko": [
      {"scope":"user","installPath":"/nonexistent/session-insights/0.0.0","version":"0.0.0"}
    ]
  }
}
JSON
REG_BEFORE_SUM="$(sha256sum "$TEST_REGISTRY" | awk '{print $1}')"

run_rollout() {
  OUT="$(CLAUDE_PLUGIN_CACHE="$TEST_CACHE" CLAUDE_PLUGIN_REGISTRY="$TEST_REGISTRY" \
    bash "$ROLLOUT" "$@" 2>&1)"
  RC=$?
}

# A single non-gate plugin: --only must be exactly that one name, not the
# whole fleet (no --canary needed since backlog/session-insights aren't gates).
run_rollout --plugin backlog --dry-run
echo "$OUT" | sed 's/^/    /'
[ "$RC" -eq 0 ] || fail "--plugin backlog --dry-run should exit 0 (got $RC)"
grep -q -- "--only=backlog " <<<"$OUT " || grep -q -- "--only=backlog$" <<<"$OUT" \
  || fail "expected rebuild-plugins.sh to be called with --only=backlog exactly"
pass "single --plugin backlog rollout scopes --only to just 'backlog'"
grep -q -- "--only=backlog,session-insights\|--only=session-insights,backlog" <<<"$OUT" \
  && fail "single-plugin rollout must not also include session-insights in --only"
pass "single-plugin rollout does not leak an unrelated plugin into --only"

# Two explicit plugins: --only must contain exactly those two, comma-joined,
# in request order — never widen to the full marketplace list.
run_rollout --plugin backlog --plugin session-insights --dry-run
echo "$OUT" | sed 's/^/    /'
[ "$RC" -eq 0 ] || fail "two-plugin --dry-run should exit 0 (got $RC)"
grep -q -- "--only=backlog,session-insights" <<<"$OUT" \
  || fail "expected --only=backlog,session-insights (exact requested set, in order)"
pass "multi --plugin rollout scopes --only to exactly the requested set"
grep -qi "blastguard\|propguard\|stuckguard" <<<"$OUT" \
  && fail "an unfiltered rollout targeting only backlog+session-insights must never mention GATE crates"
pass "GATE crate names never leak into a non-gate-targeted rollout's --only"

# --- nothing real mutated (dry-run + temp registry only) --------------------
REG_AFTER_SUM="$(sha256sum "$TEST_REGISTRY" | awk '{print $1}')"
[ "$REG_BEFORE_SUM" = "$REG_AFTER_SUM" ] || fail "temp registry changed during Part A"
pass "temp registry unchanged (Part A)"
CACHE_ENTRIES="$(find "$TEST_CACHE" -mindepth 1 2>/dev/null | wc -l | tr -d ' ')"
[ "$CACHE_ENTRIES" -eq 0 ] || fail "temp cache had $CACHE_ENTRIES entries created during Part A"
pass "temp cache untouched (Part A)"

# =============================================================================
# Part B: rebuild-plugins.sh --only actually restricts which cache bins update
# =============================================================================
echo
echo "=== Part B: rebuild-plugins.sh --only restricts the cache-refresh loop ==="

# host <os>-<arch>, matching rebuild-plugins.sh's own uname/rustc dispatch.
triple="$(rustc -vV | sed -n 's/^host: //p')"
case "$triple" in
  *apple-darwin*) os=darwin ;;
  *linux*)        os=linux ;;
  *windows*)      os=windows ;;
  *)              os=unknown ;;
esac
case "$triple" in
  x86_64-*)  arch=x86_64 ;;
  aarch64-*) arch=arm64 ;;
  *)         arch=unknown ;;
esac
SUF="$os-$arch"
# Mirror rebuild-plugins.sh's own EXT handling: cargo/rustc append .exe to
# every Windows build output, and the fixture below stands in for "a binary
# cargo just built" — an extensionless fake on a windows host does not match
# what rebuild-plugins.sh actually globs/execs, so the fixture would silently
# stop exercising Part B on Windows without this (measured 2026-08-04: Part B
# reported "cache bins scanned: 0" and failed on this host before this fix).
EXT=""
[ "$os" = windows ] && EXT=".exe"
echo "host suffix: $SUF$EXT"

# Fake "freshly built" binaries under a private CARGO_TARGET_DIR (so no real
# `cargo build` is needed — --dry-run never invokes cargo build anyway, only
# `cargo metadata` to resolve the target dir, which honors this env override).
FAKE_TARGET="$TMP/fake-target"
mkdir -p "$FAKE_TARGET/release"
printf 'FAKE-A-NEW\n' > "$FAKE_TARGET/release/backlog$EXT"
printf 'FAKE-B-NEW\n' > "$FAKE_TARGET/release/taskprog$EXT"
chmod +x "$FAKE_TARGET/release/backlog$EXT" "$FAKE_TARGET/release/taskprog$EXT"

# The two fixture plugins must be REAL repo plugins at their CURRENT
# (crates/*/.claude-plugin/plugin.json) version: rebuild-plugins.sh writes only a
# plugin's current version dir, and a cached plugin with no crate has no
# determinable current version, so it is refused and the run exits non-zero
# (backlog 8acb117a). They used to be invented names (fakepluginA/B at 1.0.0),
# which that rule now correctly rejects. Non-gate plugins, dry-run only.
plugin_version() {
  sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' \
    "$REPO/crates/$1/.claude-plugin/plugin.json" | head -1
}
VER_A="$(plugin_version backlog)"
VER_B="$(plugin_version taskprog)"
[ -n "$VER_A" ] && [ -n "$VER_B" ] || fail "could not read fixture plugin versions from crates/"

FAKE_CACHE="$TMP/fake-cache"
mkdir -p "$FAKE_CACHE/backlog/$VER_A/bin" "$FAKE_CACHE/taskprog/$VER_B/bin"
printf 'FAKE-A-OLD\n' > "$FAKE_CACHE/backlog/$VER_A/bin/backlog-$SUF$EXT"
printf 'FAKE-B-OLD\n' > "$FAKE_CACHE/taskprog/$VER_B/bin/taskprog-$SUF$EXT"
chmod +x "$FAKE_CACHE/backlog/$VER_A/bin/backlog-$SUF$EXT" "$FAKE_CACHE/taskprog/$VER_B/bin/taskprog-$SUF$EXT"

# --- with --only=backlog: only A is reported as updatable, B is skipped --
OUT="$(CARGO_TARGET_DIR="$FAKE_TARGET" CLAUDE_PLUGIN_CACHE="$FAKE_CACHE" \
  bash "$REBUILD" --no-clean --dry-run --only=backlog 2>&1)"
RC=$?
echo "$OUT" | sed 's/^/    /'
[ "$RC" -eq 0 ] || fail "rebuild-plugins.sh --only=backlog --dry-run should exit 0 (got $RC)"
grep -q "cache  would update backlog-$SUF$EXT" <<<"$OUT" \
  || fail "expected backlog to be reported as would-update"
pass "--only=backlog: targeted plugin IS reported as would-update"
grep -q "taskprog" <<<"$OUT" \
  && fail "--only=backlog must not mention taskprog at all (must be filtered out before comparison)"
pass "--only=backlog: non-targeted plugin (taskprog) is fully skipped"
grep -q "only:.*backlog.*skipped 1 bin" <<<"$OUT" \
  || fail "expected the summary line to report 'only: backlog (skipped 1 bin(s)...)'"
pass "summary line reports the --only filter and skip count"

# --- WITHOUT --only: both A and B are reported as updatable (baseline, proves
#     the default/manual/no-scope behavior is unchanged) -----------------------
OUT2="$(CARGO_TARGET_DIR="$FAKE_TARGET" CLAUDE_PLUGIN_CACHE="$FAKE_CACHE" \
  bash "$REBUILD" --no-clean --dry-run 2>&1)"
RC2=$?
echo "$OUT2" | sed 's/^/    /'
[ "$RC2" -eq 0 ] || fail "rebuild-plugins.sh --dry-run (no --only) should exit 0 (got $RC2)"
grep -q "cache  would update backlog-$SUF$EXT" <<<"$OUT2" \
  || fail "baseline (no --only): backlog should still be would-update"
grep -q "cache  would update taskprog-$SUF$EXT" <<<"$OUT2" \
  || fail "baseline (no --only): taskprog should ALSO be would-update (default = unrestricted, unchanged)"
pass "no --only (default): BOTH plugins reported as would-update — historic behavior preserved"

# --- nothing under the fake cache was actually mutated (dry-run) ------------
[ "$(cat "$FAKE_CACHE/backlog/$VER_A/bin/backlog-$SUF$EXT")" = "FAKE-A-OLD" ] \
  || fail "backlog cache binary was mutated despite --dry-run"
[ "$(cat "$FAKE_CACHE/taskprog/$VER_B/bin/taskprog-$SUF$EXT")" = "FAKE-B-OLD" ] \
  || fail "taskprog cache binary was mutated despite --dry-run"
pass "fake cache binaries untouched (--dry-run never copies)"

echo
echo "PASS: --only scoping is wired end-to-end (rollout-plugins.sh -> rebuild-plugins.sh)"
echo "      and actually restricts which cache binaries get refreshed."
