#!/usr/bin/env bash
# Self-contained test for the shared-crate rule in scripts/check-version-bumped.py.
#
# Why it exists: harness-core is statically linked into every plugin binary, so a
# change there changes ~36 shipped binaries. The bump gate only ever diffed
# crates/<plugin>/, so harness-core could move with no version anywhere moving —
# and a version that does not move when the bytes do is a version that does not
# identify what shipped (backlog 32170548). Detection already existed
# (check-plugin-rollout.py's SHARED_SOURCE_PATHS reports every plugin as drifted,
# and .deployed-from.json pins the commit); what was missing was the version.
#
# The rule chosen (over bumping all 36 plugins, which would move 108 files and
# 36 marketplace.json lines per harness-core edit, colliding with every parallel
# session): harness-core carries its OWN version, and a change to its linked
# source must bump it.
#
# This proves:
#   A. a harness-core src change with NO bump FAILS (the gap this closes);
#   B. the same change WITH a bump passes;
#   C. a harness-core tests/-only change does NOT demand a bump — integration
#      tests are not linked into any plugin binary — and says so out loud;
#   D. a harness-core Cargo.toml change (dependency edit: changes the binary)
#      with no bump FAILS;
#   E. a plugin-only change is still judged by the old rule (no regression), and
#      a harness-core change does NOT start demanding plugin bumps;
#   F. an UNPARSEABLE harness-core Cargo.toml resolves to a failure, not a pass —
#      "cannot read the version" is not "the version was bumped".
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

TMP="$(mktemp -d "${TMPDIR:-/tmp}/version-bumped-shared.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

SANDBOX="$TMP/repo"
mkdir -p "$SANDBOX/crates/foo/.claude-plugin" "$SANDBOX/crates/foo/src" \
         "$SANDBOX/crates/harness-core/src" "$SANDBOX/crates/harness-core/tests"
git -C "$SANDBOX" init -q
git -C "$SANDBOX" config user.email t@t.t
git -C "$SANDBOX" config user.name t

core_version() { # <version>
  cat >"$SANDBOX/crates/harness-core/Cargo.toml" <<EOF
[package]
name = "harness-core"
version = "$1"
edition = "2021"
EOF
}

printf '{"name":"foo","version":"1.0.0"}\n' >"$SANDBOX/crates/foo/.claude-plugin/plugin.json"
printf 'fn main() {}\n' >"$SANDBOX/crates/foo/src/main.rs"
core_version 0.2.1
printf 'pub fn a() {}\n' >"$SANDBOX/crates/harness-core/src/lib.rs"
printf 'fn t() {}\n' >"$SANDBOX/crates/harness-core/tests/it.rs"
git -C "$SANDBOX" add -A
git -C "$SANDBOX" commit -qm base

reset_tree() { git -C "$SANDBOX" checkout -q -- .; }

run_gate() { # -> stdout+stderr in $OUT, exit code in $RC
  OUT="$(cd "$SANDBOX" && python3 "$SCRIPT" 2>&1)"
  RC=$?
}

# =============================================================================
# A. harness-core src change, no bump -> must FAIL (the whole point)
# =============================================================================
printf 'pub fn a() { let _ = 1; }\n' >"$SANDBOX/crates/harness-core/src/lib.rs"
run_gate
[ "$RC" -eq 1 ] || fail "A: a harness-core src change with no bump must fail; rc=$RC out=$OUT"
case "$OUT" in
  *harness-core*) : ;;
  *) fail "A: the failure must name harness-core; out=$OUT" ;;
esac
pass "A: harness-core src change with no bump blocks"
reset_tree

# =============================================================================
# B. same change WITH a bump -> passes
# =============================================================================
printf 'pub fn a() { let _ = 1; }\n' >"$SANDBOX/crates/harness-core/src/lib.rs"
core_version 0.2.2
run_gate
[ "$RC" -eq 0 ] || fail "B: a bumped harness-core change must pass; rc=$RC out=$OUT"
pass "B: harness-core src change with a bump passes"
reset_tree

# =============================================================================
# C. tests/-only change -> no bump demanded, and the carve-out is announced
#    (structural, not a heuristic: crates/harness-core/tests/ is an integration
#    test target, never linked into a plugin binary)
# =============================================================================
printf 'fn t() { let _ = 2; }\n' >"$SANDBOX/crates/harness-core/tests/it.rs"
run_gate
[ "$RC" -eq 0 ] || fail "C: a tests-only harness-core change must not demand a bump; rc=$RC out=$OUT"
case "$OUT" in
  *"not linked"*) : ;;
  *) fail "C: the carve-out must announce itself, not exempt silently; out=$OUT" ;;
esac
pass "C: tests-only change is exempt and says so"
reset_tree

# =============================================================================
# D. Cargo.toml change (a dependency edit changes the binary), no bump -> FAIL
# =============================================================================
cat >"$SANDBOX/crates/harness-core/Cargo.toml" <<'EOF'
[package]
name = "harness-core"
version = "0.2.1"
edition = "2021"

[dependencies]
serde = "1"
EOF
run_gate
[ "$RC" -eq 1 ] || fail "D: a harness-core Cargo.toml change with no bump must fail; rc=$RC out=$OUT"
pass "D: harness-core manifest change with no bump blocks"
reset_tree

# =============================================================================
# E. no regression in either direction
# =============================================================================
printf 'fn main() { let _ = 3; }\n' >"$SANDBOX/crates/foo/src/main.rs"
run_gate
[ "$RC" -eq 1 ] || fail "E1: a plugin source change with no bump must still fail; rc=$RC out=$OUT"
pass "E1: the original plugin rule is intact"
reset_tree

# A harness-core change must NOT start demanding plugin bumps — that was the
# rejected design, and if it leaked in, every harness-core commit would need 108
# file edits.
printf 'pub fn a() { let _ = 4; }\n' >"$SANDBOX/crates/harness-core/src/lib.rs"
core_version 0.2.3
run_gate
[ "$RC" -eq 0 ] || fail "E2: a bumped harness-core change must not demand plugin bumps; rc=$RC out=$OUT"
case "$OUT" in
  *"foo: version still"*) fail "E2: harness-core must not implicate plugin foo; out=$OUT" ;;
  *) : ;;
esac
pass "E2: harness-core does not implicate the 36 plugins"
reset_tree

# =============================================================================
# F. an unreadable version resolves to FAIL, not to a pass
#    (CLAUDE.md §3: cannot-determine is not clean)
# =============================================================================
printf 'pub fn a() { let _ = 5; }\n' >"$SANDBOX/crates/harness-core/src/lib.rs"
printf 'this is not valid toml [[[\n' >"$SANDBOX/crates/harness-core/Cargo.toml"
run_gate
[ "$RC" -ne 0 ] || fail "F: an unparseable harness-core version must not pass; rc=$RC out=$OUT"
pass "F: an unreadable harness-core version fails closed"
reset_tree

echo "ALL PASS: version-bumped-shared-crate.sh"
