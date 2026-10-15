#!/usr/bin/env bash
# Regression test: the binaries rebuild-plugins.sh deploys into the plugin cache
# must be STRIPPED (no local symbol table, no debug map), and when the build
# produced an UNSTRIPPED artifact the deploy must FAIL CLOSED — exit non-zero
# and leave no unstripped binary in the cache.
#
# User requirement (2026-10-15): 「rollout binaryはstripして」.
#
# Why a test on the deployed BYTES and not on Cargo.toml: `[profile.release]
# strip = true` is a request, not an observation. It is silently defeated by
#   - CARGO_PROFILE_RELEASE_STRIP=false in the environment (overrides the
#     profile with no diagnostic), and
#   - on macOS, rustc only WARNS when the external `strip` tool fails,
# and in both cases `cargo build` exits 0. A rebuild that does not look at the
# artifact it copies therefore deploys an unstripped binary with exit 0 — the
# "could not verify" == "fine" collapse CLAUDE.md §3 forbids.
#
# This test drives the REAL path: real `cargo build` (via
# scripts/rebuild-plugins.sh --no-clean --only=<plugin>, not --dry-run, not a
# stub cargo) and a real copy into a temp CLAUDE_PLUGIN_CACHE fixture, then
# inspects the deployed file with nm/file.
#
# Stripped-ness metric (Mach-O, measured 2026-10-15 on darwin-arm64):
#   - stripped release parallelguard (strip=true, the workspace profile):
#       `nm -U` (defined symbols)          = 1   (__mh_execute_header only)
#       stab / debug-map entries (`nm -ap` type "-") = 0
#   - unstripped (CARGO_PROFILE_RELEASE_STRIP=false): measured by this test
#     at run time and printed (case B anti-vacuity), and required to exceed
#     UNSTRIPPED_MIN so case B cannot pass against a stripped source.
# Threshold: a deployed binary is "stripped" iff defined symbols <= 1 AND
# stab entries == 0.
#
# Cases (each against BOTH deploy paths of rebuild-plugins.sh):
#   refresh path: fixture version dir already holds <bin>-<os>-<arch>
#                 (main refresh loop overwrites it when bytes differ)
#   seed path:    fresh version dir holding only the launcher bin/<bin>
#                 (seed loop places the host binary)
#   A) normal build  -> exit 0, deployed binary is stripped.
#   B) STRIP=false   -> exit non-zero, and the cache holds NO unstripped
#                       binary (refresh: placeholder still stripped; seed:
#                       host bin absent or stripped).
#
# Cost: rebuild-plugins.sh always builds the WHOLE workspace (LTO), twice
# (once per case, in separate target dirs so reruns are incremental). Expect
# minutes on a cold target dir. Target dirs default to
# $REPO/target/strip-test-{stripped,unstripped} (gitignored); override the
# parent with STRIP_TEST_TARGET_ROOT.
#
# Platform: the metric is Mach-O specific. On any other host this test does
# NOT pass — it exits 2 ("could not determine"), never 0.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
REBUILD="$REPO/scripts/rebuild-plugins.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"

PLUGIN=parallelguard
BIN=parallelguard
UNSTRIPPED_MIN=100   # an unstripped Rust binary has thousands; 100 is a floor

case "$(uname -s)-$(uname -m)" in
  Darwin-arm64)  SUF=darwin-arm64 ;;
  Darwin-x86_64) SUF=darwin-x86_64 ;;
  *) echo "UNDETERMINED: stripped-ness metric is Mach-O specific; host $(uname -s)-$(uname -m) not supported" >&2; exit 2 ;;
esac
command -v nm >/dev/null || { echo "UNDETERMINED: nm not found" >&2; exit 2; }

# A rollout lock holder inherited from a parent rollout would make
# rebuild-plugins.sh refuse the temp cache; this test is standalone.
unset HARNESS_ROLLOUT_LOCK_HOLDER_PID

PJ="$REPO/crates/$PLUGIN/.claude-plugin/plugin.json"
VER="$(sed -n 's/.*"version"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p' "$PJ" | head -1)"
[ -n "$VER" ] || fail "could not read version from $PJ"
LAUNCHER="$REPO/crates/$PLUGIN/bin/$BIN"
[ -f "$LAUNCHER" ] || fail "launcher $LAUNCHER missing"

TARGET_ROOT="${STRIP_TEST_TARGET_ROOT:-$REPO/target}"
TD_STRIPPED="$TARGET_ROOT/strip-test-stripped"
TD_UNSTRIPPED="$TARGET_ROOT/strip-test-unstripped"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/rebuild-stripped.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# defined-symbol count; prints "ERR" if nm fails (never a plausible number)
defined_syms() {
  local out
  out="$(nm -U "$1" 2>/dev/null)" || { echo ERR; return; }
  printf '%s\n' "$out" | grep -c . || true
}
# stab / debug-map entries (N_OSO, N_SO, N_FUN ...): type column "-"
stab_syms() {
  local out
  out="$(nm -ap "$1" 2>/dev/null)" || { echo ERR; return; }
  printf '%s\n' "$out" | awk '$2=="-"' | grep -c . || true
}
is_stripped() {   # 0 = stripped, 1 = not stripped / undeterminable
  local f="$1" d s
  file -b "$f" | grep -q 'Mach-O' || return 1
  d="$(defined_syms "$f")"; s="$(stab_syms "$f")"
  [ "$d" != ERR ] && [ "$s" != ERR ] || return 1
  [ "$d" -le 1 ] && [ "$s" -eq 0 ]
}
describe() { echo "$(file -b "$1" | cut -c1-40) defined=$(defined_syms "$1") stabs=$(stab_syms "$1")"; }

# make_cache <dir> <refresh|seed> [placeholder]
make_cache() {
  local cache="$1" mode="$2" ph="${3:-}"
  local bindir="$cache/$PLUGIN/$VER/bin"
  mkdir -p "$bindir"
  cp "$LAUNCHER" "$bindir/$BIN"; chmod +x "$bindir/$BIN"
  if [ "$mode" = refresh ]; then
    cp "$ph" "$bindir/$BIN-$SUF"; chmod +x "$bindir/$BIN-$SUF"
  fi
}

# run_rebuild <cache> <target_dir> [extra env...] ; sets RC and OUT
run_rebuild() {
  local cache="$1" td="$2"; shift 2
  OUT="$(cd "$REPO" && env "$@" CARGO_TARGET_DIR="$td" CLAUDE_PLUGIN_CACHE="$cache" \
    bash "$REBUILD" --no-clean --only="$PLUGIN" 2>&1)"
  RC=$?
}

# ---------------------------------------------------------------- case A ----
echo "== case A: normal build (workspace profile, strip = true)"
# refresh path: placeholder is a non-Mach-O stub so the bytes differ and the
# main loop MUST overwrite it.
printf '#!/bin/sh\necho placeholder\n' >"$TMP/stub"
CA_R="$TMP/A-refresh/cache"
make_cache "$CA_R" refresh "$TMP/stub"
run_rebuild "$CA_R" "$TD_STRIPPED"
[ "$RC" = 0 ] || { echo "$OUT" >&2; fail "A/refresh: rebuild exited $RC on a normal build"; }
SRC_A="$TD_STRIPPED/release/$BIN"
[ -x "$SRC_A" ] || { echo "$OUT" >&2; fail "A: build artifact $SRC_A missing"; }
echo "     source:   $SRC_A: $(describe "$SRC_A")"
DEP="$CA_R/$PLUGIN/$VER/bin/$BIN-$SUF"
cmp -s "$SRC_A" "$DEP" || { echo "$OUT" >&2; fail "A/refresh: deployed binary is not the build artifact (copy did not happen)"; }
echo "     deployed: $DEP: $(describe "$DEP")"
is_stripped "$DEP" || fail "A/refresh: deployed binary is NOT stripped: $(describe "$DEP")"
pass "A/refresh: exit 0, deployed binary is the build artifact and is stripped"

CA_S="$TMP/A-seed/cache"
make_cache "$CA_S" seed
run_rebuild "$CA_S" "$TD_STRIPPED"
[ "$RC" = 0 ] || { echo "$OUT" >&2; fail "A/seed: rebuild exited $RC on a normal build"; }
DEP="$CA_S/$PLUGIN/$VER/bin/$BIN-$SUF"
[ -f "$DEP" ] || { echo "$OUT" >&2; fail "A/seed: host binary was not seeded"; }
is_stripped "$DEP" || fail "A/seed: seeded binary is NOT stripped: $(describe "$DEP")"
pass "A/seed: exit 0, seeded binary is stripped"

# Keep a known-stripped real binary as the case-B refresh placeholder.
cp "$SRC_A" "$TMP/stripped-placeholder"

# ---------------------------------------------------------------- case B ----
echo "== case B: unstripped build (CARGO_PROFILE_RELEASE_STRIP=false)"
CB_R="$TMP/B-refresh/cache"
make_cache "$CB_R" refresh "$TMP/stripped-placeholder"
run_rebuild "$CB_R" "$TD_UNSTRIPPED" CARGO_PROFILE_RELEASE_STRIP=false
RC_BR=$RC OUT_BR="$OUT"
SRC_B="$TD_UNSTRIPPED/release/$BIN"
# Anti-vacuity: the SOURCE really is unstripped, else case B proves nothing.
[ -x "$SRC_B" ] || { echo "$OUT_BR" >&2; fail "B: build artifact $SRC_B missing — fixture error, not a verdict"; }
SB_DEF="$(defined_syms "$SRC_B")"
echo "     source:   $SRC_B: $(describe "$SRC_B")"
[ "$SB_DEF" != ERR ] && [ "$SB_DEF" -ge "$UNSTRIPPED_MIN" ] \
  || fail "B: source artifact is not unstripped (defined=$SB_DEF < $UNSTRIPPED_MIN) — fixture is vacuous"
pass "B: source artifact is unstripped (defined symbols = $SB_DEF) — fixture is not vacuous"

DEP="$CB_R/$PLUGIN/$VER/bin/$BIN-$SUF"
echo "     rebuild exit: $RC_BR"
echo "     deployed: $DEP: $(describe "$DEP")"
B_FAIL=""
[ "$RC_BR" != 0 ] || B_FAIL="$B_FAIL [refresh: rebuild exited 0 on an unstripped build]"
if [ -e "$DEP" ] && ! is_stripped "$DEP"; then
  B_FAIL="$B_FAIL [refresh: cache now holds an UNSTRIPPED binary: $(describe "$DEP")]"
fi

CB_S="$TMP/B-seed/cache"
make_cache "$CB_S" seed
run_rebuild "$CB_S" "$TD_UNSTRIPPED" CARGO_PROFILE_RELEASE_STRIP=false
RC_BS=$RC OUT_BS="$OUT"
DEP="$CB_S/$PLUGIN/$VER/bin/$BIN-$SUF"
echo "     seed rebuild exit: $RC_BS"
[ -e "$DEP" ] && echo "     seeded:   $DEP: $(describe "$DEP")" || echo "     seeded:   (absent)"
[ "$RC_BS" != 0 ] || B_FAIL="$B_FAIL [seed: rebuild exited 0 on an unstripped build]"
if [ -e "$DEP" ] && ! is_stripped "$DEP"; then
  B_FAIL="$B_FAIL [seed: cache now holds an UNSTRIPPED binary: $(describe "$DEP")]"
fi

if [ -n "$B_FAIL" ]; then
  echo "--- rebuild output (refresh) ---" >&2; echo "$OUT_BR" | tail -15 >&2
  echo "--- rebuild output (seed) ---" >&2;    echo "$OUT_BS" | tail -15 >&2
  fail "B: unstripped build was not refused:$B_FAIL"
fi
pass "B: unstripped build refused (refresh exit $RC_BR, seed exit $RC_BS) and no unstripped binary reached the cache"

echo "PASS: rebuild-plugins.sh deploys only stripped binaries and fails closed on an unstripped build"
