#!/usr/bin/env bash
# scripts/tests/clippy-lints-optin-verdict-crates.sh
#
# Pins backlog 034b6620: the REAL crates `tdd` (crates/tdd) and `condukt`
# (crates/condukt) must be covered by scripts/check-clippy-lints.py — i.e. a
# denied `.unwrap()` planted in their PRODUCTION (non-test) code must make
# the gate block (exit 1), not "not opted in" / "nothing to check" (exit 0).
#
# Why this is a SEPARATE test from scripts/tests/precommit-clippy-gate.sh
# ------------------------------------------------------------------------
# That suite already proves the opt-in MECHANISM works, using synthetic
# fixture crates (`gatecrate`, `plaincrate`) built from scratch. It does not,
# and cannot, tell you whether any particular real crate in THIS repository
# has actually opted in. Membership in scripts/check-clippy-lints.py is
# derived from `[lints] workspace = true` in each crate's own Cargo.toml
# (see that scanner's module docstring, "Membership is DERIVED, never
# hardcoded") — a crate that never added that line is invisible to the gate
# no matter how correct the gate's derivation logic is. tdd and condukt are
# both verdict-bearing gate crates (tdd blocks Stop on missing tests; condukt
# orchestrates and merges work across worktrees), so an unwrap()-driven panic
# inside either is exactly the "cannot-determine turned into a crash instead
# of a block" case CLAUDE.md section 3 names. This test exercises the REAL
# crates, in a sandbox copy of the REAL repository, to answer "are these two
# specific crates actually covered" as an observed fact, not an inference
# from the mechanism being correct in the abstract.
#
# What each case proves
# ----------------------
#   planted  — appends a production (non-test) item containing a denied
#              `.unwrap()` to crates/<crate>/src/main.rs, runs the scanner,
#              and requires exit 1 AND a
#              "running: cargo clippy -p <crate> --all-targets" line in the
#              output (proving the crate was actually compiled and checked,
#              not blocked for some unrelated UNDETERMINED reason such as a
#              missing cargo).
#   control  — anti-vacuity. From a FRESH reset of the same sandbox (never
#              the post-"planted" state), appends a genuinely clean
#              production item to the same file and requires exit 0 AND the
#              same "running: cargo clippy -p <crate>" line. Without the
#              second assertion here, a scanner that silently SKIPS the
#              crate (because it is not opted in) would satisfy "exit 0"
#              vacuously — it would look identical to a scanner that checked
#              the crate and found it clean. The "running:" line is what
#              tells the two apart.
#
# EXPECTED RESULT AT THE TIME THIS TEST WAS WRITTEN (backlog 034b6620, not
# yet fixed): neither crates/tdd/Cargo.toml nor crates/condukt/Cargo.toml
# carries `[lints] workspace = true`. So for BOTH crates: "planted" is
# expected to FAIL (the scanner prints "not opted in ([lints] workspace =
# true absent): crates/<dir>" and exits 0, not 1), and "control" is expected
# to FAIL on its "running:" assertion (exit 0 is correct, but for the wrong
# reason — nothing was ever compiled). Per CLAUDE.md section 2 ("tests must
# be observed RED before GREEN"), this script is committed to being run and
# read while red; do not edit crates/tdd or crates/condukt to silence it.
#
# Sandbox construction — copy of the WORKING TREE, not HEAD
# -----------------------------------------------------------
# `git ls-files -z -co --exclude-standard` (cached + others, NUL-separated,
# gitignore-respecting) enumerates exactly what is actually on disk right
# now in this worktree — tracked files as they currently stand, plus any
# untracked-but-not-ignored files — mirroring how
# scripts/check-clippy-lints.py itself selects (working tree, never the
# index alone; see that scanner's "diff source == scanned content"
# invariant). `target/` is excluded twice over: it is gitignored (so
# --exclude-standard already drops it) and the copy step additionally
# refuses any path starting with `target/` or containing `/target/` as a
# belt-and-braces measure. The whole workspace is copied (not just
# crates/tdd or crates/condukt) because `[workspace] members = ["crates/*"]`
# in the root Cargo.toml means `cargo clippy -p tdd` needs every member
# crate present to resolve the workspace at all — tdd and condukt both have
# in-tree path dependencies (harness-core, overwatch, blastguard,
# schemaguard, ...) that must exist on disk.
#
# The copy is then `git init` + committed inside the sandbox so the scanner
# (which needs a repo root and a HEAD to diff against — see
# scripts/check-clippy-lints.py's `changed_paths`) has one to work with.
#
# This script NEVER mutates the real repository. Every git command below
# that is destructive (reset --hard, commit, etc.) targets `$SANDBOX`, a
# directory under a fresh `mktemp -d`, not the worktree this script lives
# in. "Do not run git stash/checkout/reset in the worktree" (this session's
# instructions) refers to /Users/yuki/src/.harness-worktrees/lints-034b6620
# itself; $SANDBOX is a disposable throwaway copy this script owns outright,
# cleaned up by the EXIT trap.
#
# MAC-RUNNABLE: bash 3.2, BSD tools, python3 (used only for the tree copy,
# so file/symlink handling is uniform regardless of bsdtar/cpio version
# quirks), git, cargo. No network access. No file in the real repository is
# read for anything but the initial snapshot listing and is never written.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCANNER="$REPO/scripts/check-clippy-lints.py"
REAL_GIT="$(command -v git)"
PY="$(command -v python3)"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

[ -n "$REAL_GIT" ] || fail "git not found on PATH"
[ -n "$PY" ] || fail "python3 not found on PATH"
[ -f "$SCANNER" ] || fail "scripts/check-clippy-lints.py does not exist (nothing to test)"

REAL_CARGO=""
if command -v cargo >/dev/null 2>&1; then
  REAL_CARGO="$(command -v cargo)"
elif [ -x "$HOME/.cargo/bin/cargo" ]; then
  REAL_CARGO="$HOME/.cargo/bin/cargo"
fi
[ -n "$REAL_CARGO" ] || fail "no cargo found (PATH or ~/.cargo/bin) — this test needs the real toolchain"
echo "using repo:    $REPO"
echo "using scanner: $SCANNER"
echo "using cargo:   $REAL_CARGO"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/clippy-lints-optin-verdict.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# Shared build cache across all four (2 crates x 2 cases) real-clippy runs.
# Honour an external override if one is set, so a caller can point this at a
# persistent cache across repeated runs of this script.
: "${CLIPPY_LINTS_TEST_TARGET_DIR:=$TMP/cargo-target}"
mkdir -p "$CLIPPY_LINTS_TEST_TARGET_DIR"
export CARGO_TARGET_DIR="$CLIPPY_LINTS_TEST_TARGET_DIR"

SANDBOX="$TMP/sandbox"
mkdir -p "$SANDBOX"

echo
echo ">>> building sandbox copy of the CURRENT WORKING TREE"
FILELIST="$TMP/filelist.nul"
( cd "$REPO" && "$REAL_GIT" ls-files -z -co --exclude-standard ) >"$FILELIST"
[ -s "$FILELIST" ] || fail "git ls-files produced no entries — nothing to copy"

"$PY" - "$REPO" "$SANDBOX" "$FILELIST" <<'PYEOF'
import os
import shutil
import sys

repo, dest, listfile = sys.argv[1], sys.argv[2], sys.argv[3]

with open(listfile, "rb") as f:
    raw = f.read()

paths = [p for p in raw.split(b"\x00") if p]
copied = 0
for raw_path in paths:
    path = raw_path.decode("utf-8", "surrogateescape")
    if path.startswith("target/") or "/target/" in path:
        continue
    src = os.path.join(repo, path)
    dst = os.path.join(dest, path)
    os.makedirs(os.path.dirname(dst), exist_ok=True)
    if os.path.islink(src):
        os.symlink(os.readlink(src), dst)
    elif os.path.isfile(src):
        shutil.copy2(src, dst)
    else:
        continue
    copied += 1

if copied == 0:
    sys.exit("copied zero files out of the working tree")
print(f"copied {copied} files into sandbox")
PYEOF
[ -f "$SANDBOX/Cargo.toml" ] || fail "sandbox copy has no root Cargo.toml — copy step is broken"
[ -f "$SANDBOX/crates/tdd/src/main.rs" ] || fail "sandbox copy is missing crates/tdd/src/main.rs"
[ -f "$SANDBOX/crates/condukt/src/main.rs" ] || fail "sandbox copy is missing crates/condukt/src/main.rs"
[ -f "$SANDBOX/scripts/check-clippy-lints.py" ] || fail "sandbox copy is missing the scanner itself"
pass "sandbox tree materialized"

"$REAL_GIT" -C "$SANDBOX" init -q
"$REAL_GIT" -C "$SANDBOX" config user.email t@t.t
"$REAL_GIT" -C "$SANDBOX" config user.name t
"$REAL_GIT" -C "$SANDBOX" config commit.gpgsign false
"$REAL_GIT" -C "$SANDBOX" add -A >/dev/null
"$REAL_GIT" -C "$SANDBOX" commit -qm "sandbox baseline (copy of working tree)"
BASE_COMMIT="$("$REAL_GIT" -C "$SANDBOX" rev-parse HEAD)"
pass "sandbox git repo initialized at $BASE_COMMIT"

reset_sandbox() {
  "$REAL_GIT" -C "$SANDBOX" reset -q --hard "$BASE_COMMIT"
  "$REAL_GIT" -C "$SANDBOX" clean -q -fdx -- crates >/dev/null
}

OUT=""; RC=""
scan() {
  OUT="$( cd "$SANDBOX" && env PATH="$PATH" "$PY" "$SCANNER" 2>&1; echo "RC=$?" )"
  RC="$(printf '%s\n' "$OUT" | grep -o 'RC=[0-9]*$' | tail -1 | cut -d= -f2)"
  printf '%s\n' "$OUT" | sed 's/^/    /'
}

OVERALL_FAIL=0
note_fail() {
  echo "FAIL: $*" >&2
  OVERALL_FAIL=1
}

MAIN_REL_PATH_FOR() { # <crate> -> repo-relative path to its main.rs
  echo "crates/$1/src/main.rs"
}

run_planted_case() { # <crate>
  local crate="$1" main_rel
  main_rel="$(MAIN_REL_PATH_FOR "$crate")"
  echo
  echo ">>> $crate / planted: denied .unwrap() in production code — expect exit 1"
  reset_sandbox
  cat >>"$SANDBOX/$main_rel" <<'RSEOF'

#[allow(dead_code)]
fn __planted_034b6620() -> u8 {
    "7".parse::<u8>().ok().unwrap()
}
RSEOF
  scan
  local running_seen=0
  printf '%s\n' "$OUT" | grep -q "running: cargo clippy -p $crate --all-targets" && running_seen=1

  if [ "$RC" != "1" ]; then
    note_fail "$crate/planted: expected exit 1 (blocked), got $RC — scanner output above"
  else
    pass "$crate/planted: exit 1"
  fi
  if [ "$running_seen" != "1" ]; then
    note_fail "$crate/planted: no 'running: cargo clippy -p $crate --all-targets' line — crate was not actually checked (likely still not opted in: [lints] workspace = true missing from crates/$crate/Cargo.toml)"
  else
    pass "$crate/planted: crate was actually compiled and checked"
  fi
}

run_control_case() { # <crate>  -- anti-vacuity: same file, genuinely clean change
  local crate="$1" main_rel
  main_rel="$(MAIN_REL_PATH_FOR "$crate")"
  echo
  echo ">>> $crate / control (anti-vacuity): clean production change — expect exit 0 AND checked"
  reset_sandbox
  cat >>"$SANDBOX/$main_rel" <<'RSEOF'

#[allow(dead_code)]
fn __clean_034b6620() -> u8 {
    7
}
RSEOF
  scan
  local running_seen=0
  printf '%s\n' "$OUT" | grep -q "running: cargo clippy -p $crate --all-targets" && running_seen=1

  if [ "$RC" != "0" ]; then
    note_fail "$crate/control: expected exit 0 (clean), got $RC — scanner output above"
  else
    pass "$crate/control: exit 0"
  fi
  if [ "$running_seen" != "1" ]; then
    note_fail "$crate/control: no 'running: cargo clippy -p $crate --all-targets' line — exit 0 is VACUOUS (the crate was skipped, not checked-and-clean); this is the anti-vacuity check catching a silent skip"
  else
    pass "$crate/control: crate was actually compiled and checked (exit 0 is a real pass, not a skip)"
  fi
}

for crate in tdd condukt; do
  run_planted_case "$crate"
  run_control_case "$crate"
done

echo
if [ "$OVERALL_FAIL" != "0" ]; then
  echo "FAIL: backlog 034b6620 is NOT pinned clean — tdd and/or condukt are not"
  echo "      (fully) covered by scripts/check-clippy-lints.py. See the FAIL"
  echo "      lines above for which assertion(s) failed per crate/case."
  exit 1
fi

echo "PASS: tdd and condukt are both covered by scripts/check-clippy-lints.py —"
echo "      a denied .unwrap() planted in either crate's production code blocks"
echo "      (exit 1), and a genuinely clean change to either is checked and"
echo "      passes (exit 0), never a silent skip."
