#!/usr/bin/env bash
# Self-contained test for the derived-artifact carve-out in
# scripts/check-version-bumped.py.
#
# Why it exists: the CI workflow "rebuild plugin binaries" commits regenerated
# crates/<name>/bin/* with NO source change and no version bump. GitHub Actions
# does not use core.hooksPath, so that commit lands remotely; locally the gate
# then refuses to integrate it (observed 2026-07-23: `git merge origin/main`
# blocked with "VERSION-BUMP GATE FAILED: 34 changed plugin(s) not bumped").
# Those byte differences are build nondeterminism — the same class this repo
# already declares unusable for a verdict in scripts/check-bin-reproducibility.py
# — so demanding a version bump for them demands a bump for a non-change.
#
# This proves:
#   A. a bin-ONLY change no longer demands a bump (exit 0), and says so;
#   B. a source change with no bump STILL fails (exit 1) — the gate is intact;
#   C. bin + source together with no bump STILL fails (exit 1) — the carve-out
#      must not swallow a real change that merely travels alongside binaries;
#   D. a bin-only change WITH a bump still passes (exit 0, no regression).
#
# MAC-RUNNABLE: pure git + python3 in a temp repo sandbox; no GNU coreutils, no
# network, no real ~/.claude touched.
#
# Exit 0 on success, non-zero on any failed assertion.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$REPO/scripts/check-version-bumped.py"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/version-bumped-bin-only.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

SANDBOX="$TMP/repo"
mkdir -p "$SANDBOX/crates/foo/.claude-plugin" "$SANDBOX/crates/foo/bin" "$SANDBOX/crates/foo/src"
git -C "$SANDBOX" init -q
git -C "$SANDBOX" config user.email t@t.t
git -C "$SANDBOX" config user.name t

write_version() { # <version>
  printf '{"name":"foo","version":"%s"}\n' "$1" >"$SANDBOX/crates/foo/.claude-plugin/plugin.json"
}

write_version 1.0.0
printf 'BIN-V1\n' >"$SANDBOX/crates/foo/bin/foo-darwin-arm64"
printf 'fn main() {}\n' >"$SANDBOX/crates/foo/src/main.rs"
git -C "$SANDBOX" add -A
git -C "$SANDBOX" commit -qm base

# Reset the working tree to the committed state between cases.
reset_tree() {
  git -C "$SANDBOX" checkout -q -- .
}

run_gate() { # -> stdout+stderr in $OUT, exit code in $RC
  OUT="$(cd "$SANDBOX" && python3 "$SCRIPT" 2>&1)"
  RC=$?
}

# =============================================================================
# A. bin-ONLY change, no bump -> must PASS (this is the carve-out)
# =============================================================================
printf 'BIN-V2-REBUILT\n' >"$SANDBOX/crates/foo/bin/foo-darwin-arm64"
run_gate
[ "$RC" -eq 0 ] || fail "A: a bin-only rebuild must not demand a bump; rc=$RC out=$OUT"
case "$OUT" in
  *"derived-artifact"*) : ;;
  *) fail "A: the carve-out must announce itself, not exempt silently; out=$OUT" ;;
esac
pass "A: bin-only rebuild passes and the exemption is announced"
reset_tree

# =============================================================================
# B. source change, no bump -> must STILL FAIL
# =============================================================================
printf 'fn main() { let _ = 1; }\n' >"$SANDBOX/crates/foo/src/main.rs"
run_gate
[ "$RC" -eq 1 ] || fail "B: a source change with no bump must still fail; rc=$RC out=$OUT"
pass "B: source change with no bump still blocks"
reset_tree

# =============================================================================
# C. bin + source together, no bump -> must STILL FAIL
# =============================================================================
printf 'BIN-V3\n' >"$SANDBOX/crates/foo/bin/foo-darwin-arm64"
printf 'fn main() { let _ = 2; }\n' >"$SANDBOX/crates/foo/src/main.rs"
run_gate
[ "$RC" -eq 1 ] || fail "C: bin+source with no bump must still fail; rc=$RC out=$OUT"
pass "C: the carve-out does not swallow a source change travelling with binaries"
reset_tree

# =============================================================================
# D. bin-only change WITH a bump -> still passes (no regression)
# =============================================================================
printf 'BIN-V4\n' >"$SANDBOX/crates/foo/bin/foo-darwin-arm64"
write_version 1.0.1
run_gate
[ "$RC" -eq 0 ] || fail "D: a bumped bin-only change must pass; rc=$RC out=$OUT"
pass "D: bin-only change with a bump still passes"
reset_tree

echo "ALL PASS: version-bumped-bin-only.sh"
