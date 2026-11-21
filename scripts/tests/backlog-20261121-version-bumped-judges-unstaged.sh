#!/usr/bin/env bash
# backlog RED (2026-11-21, condukt run run-20261121-114054-75519): in its
# pre-commit mode (no --head) scripts/check-version-bumped.py judges the
# WORKING TREE, not the content being committed.
#
# Observed in worktree run-20261121-114054-75519-bg9236: during a commit the
# bump gate's "changed files" list included UNSTAGED rustfmt edits to two
# blastguard test files that were not part of the commit. `_range()` (docstring
# "`git diff` operands: working tree vs base, or base vs head.") makes the
# pre-commit diff `git diff --name-only HEAD`, which includes unstaged edits,
# and the current version is read from the working-tree plugin.json.
#
# Two readings (both recorded; this script asserts the first):
#   (1) defect: a pre-commit gate judges the commit (the index), like the
#       `--staged` scanners (.githooks/pre-commit, user ruling 2026-11-08
#       "the scanner checks only this commit's range");
#   (2) intended: the gate's header documents "pre-commit: working tree vs
#       last commit".
# Under (1) both directions are wrong:
#   A. false positive: only an UNSTAGED edit touches plugin foo and the commit
#      stages an unrelated file -> the gate must pass (exit 0); it blocks.
#   B. fail-open: a STAGED source change to foo with the version bump left
#      UNSTAGED -> the commit carries no bump, the gate must block; it passes.
# Controls (must hold today, else the fixture is broken -> exit 2):
#   C. staged change + staged bump -> exit 0.
#   D. staged change, no bump anywhere -> exit 1.
#
# Exit: 0 = both A and B behave as reading (1); 1 = RED; 2 = control failed.
# MAC-RUNNABLE: pure git + python3 in a temp repo sandbox.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$REPO/scripts/check-version-bumped.py"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/vbump-unstaged.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

export GIT_CONFIG_GLOBAL=/dev/null GIT_CONFIG_NOSYSTEM=1
export GIT_AUTHOR_NAME=t GIT_AUTHOR_EMAIL=t@t.t GIT_COMMITTER_NAME=t GIT_COMMITTER_EMAIL=t@t.t

# fresh <dir>: plugin foo 1.0.0 with src/main.rs, plus docs/a.txt, committed.
fresh() {
  local d="$1"
  mkdir -p "$d/crates/foo/.claude-plugin" "$d/crates/foo/src" "$d/docs"
  git -C "$d" init -q -b main
  printf '{"name":"foo","version":"1.0.0"}\n' > "$d/crates/foo/.claude-plugin/plugin.json"
  printf 'fn main() {}\n' > "$d/crates/foo/src/main.rs"
  printf 'one\n' > "$d/docs/a.txt"
  git -C "$d" add -A
  git -C "$d" commit -qm base
}

bump() { printf '{"name":"foo","version":"1.0.1"}\n' > "$1/crates/foo/.claude-plugin/plugin.json"; }
edit_src() { printf 'fn main() { let _ = 1; }\n' > "$1/crates/foo/src/main.rs"; }

gate() { # <dir>; sets RC, OUT
  OUT="$(cd "$1" && python3 "$SCRIPT" 2>&1)"
  RC=$?
}

red=0
ctl=0

# C (control)
d="$TMP/c"; fresh "$d"; edit_src "$d"; bump "$d"; git -C "$d" add -A
gate "$d"
if [ "$RC" -ne 0 ]; then echo "CONTROL FAIL C: staged change + staged bump must pass; rc=$RC out=$OUT"; ctl=1; else echo "  ok C: staged change + staged bump passes"; fi

# D (control)
d="$TMP/d"; fresh "$d"; edit_src "$d"; git -C "$d" add -A
gate "$d"
if [ "$RC" -ne 1 ]; then echo "CONTROL FAIL D: staged change without bump must block; rc=$RC out=$OUT"; ctl=1; else echo "  ok D: staged change without bump blocks"; fi

# A (false positive)
d="$TMP/a"; fresh "$d"; edit_src "$d"; printf 'two\n' > "$d/docs/a.txt"; git -C "$d" add docs/a.txt
gate "$d"
if [ "$RC" -ne 0 ]; then
  echo "RED A: only an UNSTAGED edit touches plugin foo, the commit stages docs/a.txt only, yet the gate blocked; rc=$RC"
  printf '%s\n' "$OUT" | sed 's/^/    /'
  red=1
else
  echo "  ok A: unstaged edit outside the commit is not judged"
fi

# B (fail-open)
d="$TMP/b"; fresh "$d"; edit_src "$d"; git -C "$d" add crates/foo/src/main.rs; bump "$d"
gate "$d"
if [ "$RC" -eq 0 ]; then
  echo "RED B: the commit stages a foo source change WITHOUT a bump (the bump is unstaged), yet the gate passed; rc=$RC"
  printf '%s\n' "$OUT" | sed 's/^/    /'
  red=1
else
  echo "  ok B: unstaged bump does not certify a staged change"
fi

[ "$ctl" -ne 0 ] && exit 2
[ "$red" -ne 0 ] && exit 1
echo "all ok"
exit 0
