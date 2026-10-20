#!/usr/bin/env bash
# Test for the same-version-content-change ("version collision") detection:
#   - scripts/check-version-bumped.py --base A --head B  (commit-vs-commit mode)
#   - .githooks/post-merge  (runs that check after every merge, ff included)
#
# Incident: two branches both bump plugin p 0.1.0 -> 0.1.1 with DIFFERENT
# content. Each passes the bump gate against its own parent, git merges the
# identical version line without conflict, and the merged tree has one version
# naming two contents. A rollout at an unchanged version is a no-op.
#
# Cases:
#   a. checker --head: same-version content change -> 1 naming p; bumped -> 0;
#      unchanged -> 0; bogus ref -> 2
#   b. checker without --head still judges the working tree
#   c. hook, fast-forward onto a same-version content change -> VERSION COLLISION
#   d. hook, true merge of the incident -> VERSION COLLISION
#   e. hook, clean bump (ff and --no-ff) -> neither VERSION COLLISION nor UNDETERMINED
#
# Exit 0 on success, non-zero on any failed assertion.
set -u

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
CHECKER="$REPO/scripts/check-version-bumped.py"
HOOK="$REPO/.githooks/post-merge"

FAILS=0
pass() { echo "PASS: $*"; }
fail() { echo "FAIL: $*" >&2; FAILS=$((FAILS + 1)); }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/post-merge-collision.XXXXXX")"

# new_repo -> prints path of a repo with p 0.1.0 committed on main
new_repo() {
  local d
  d="$(mktemp -d "$TMP/r.XXXXXX")"
  mkdir -p "$d/crates/p/.claude-plugin" "$d/scripts"
  git -C "$d" init -q -b main
  git -C "$d" config user.email t@t.t
  git -C "$d" config user.name t
  git -C "$d" config commit.gpgsign false
  cp "$CHECKER" "$d/scripts/check-version-bumped.py"
  cp "$HOOK" "$d/.git/hooks/post-merge"
  chmod +x "$d/.git/hooks/post-merge"
  printf '#!/bin/sh\nexit 0\n' >"$d/.git/hooks/post-commit"
  chmod +x "$d/.git/hooks/post-commit"
  setv "$d" 0.1.0
  echo base >"$d/crates/p/README.md"
  git -C "$d" add -A
  git -C "$d" commit -q -m base
  echo "$d"
}

setv() { # <repo> <version>
  printf '{"version": "%s"}\n' "$2" >"$1/crates/p/.claude-plugin/plugin.json"
}

commit_all() { # <repo> <msg>
  git -C "$1" add -A
  git -C "$1" commit -q -m "$2"
}

run_checker() { # <repo> <args...>; sets RC, ERR
  local d="$1"; shift
  ERR="$(cd "$d" && python3 scripts/check-version-bumped.py "$@" 2>&1 >/dev/null)"
  RC=$?
}

# ---------------------------------------------------------------- a
d="$(new_repo)"
setv "$d" 0.1.1; echo A >"$d/crates/p/README.md"; commit_all "$d" x
X="$(git -C "$d" rev-parse HEAD)"
echo B >"$d/crates/p/README.md"; commit_all "$d" y-samever
Y="$(git -C "$d" rev-parse HEAD)"
setv "$d" 0.1.2; echo C >"$d/crates/p/README.md"; commit_all "$d" z-bumped
Z="$(git -C "$d" rev-parse HEAD)"
echo unrelated >"$d/other.txt"; commit_all "$d" w-unrelated
W="$(git -C "$d" rev-parse HEAD)"

run_checker "$d" --base "$X" --head "$Y"
if [ "$RC" -eq 1 ] && printf '%s' "$ERR" | grep -qw p; then pass "a1 same-version content change -> exit 1 naming p"
else fail "a1 expected exit 1 naming p, got rc=$RC err=[$ERR]"; fi

run_checker "$d" --base "$Y" --head "$Z"
if [ "$RC" -eq 0 ]; then pass "a2 bumped -> exit 0"; else fail "a2 expected 0, got rc=$RC err=[$ERR]"; fi

run_checker "$d" --base "$Z" --head "$W"
if [ "$RC" -eq 0 ]; then pass "a3 unchanged plugin -> exit 0"; else fail "a3 expected 0, got rc=$RC err=[$ERR]"; fi

run_checker "$d" --base "$X" --head deadbeefnotaref
if [ "$RC" -eq 2 ]; then pass "a4 bogus head ref -> exit 2"; else fail "a4 expected 2, got rc=$RC err=[$ERR]"; fi
run_checker "$d" --base deadbeefnotaref --head "$Y"
if [ "$RC" -eq 2 ]; then pass "a5 bogus base ref -> exit 2"; else fail "a5 expected 2, got rc=$RC err=[$ERR]"; fi

# ---------------------------------------------------------------- b
# Working tree (no --head): HEAD is W (p 0.1.2); edit uncommitted, no bump.
# Base for the old mode is whatever the script defaults/accepts; pass --base HEAD
# if supported, else rely on defaults. Both are attempted; the case asserts exit 1.
echo dirty >"$d/crates/p/README.md"
run_checker "$d" --base "$W"
RC_B=$RC; ERR_B="$ERR"
if [ "$RC_B" -eq 1 ]; then pass "b uncommitted unbumped edit, no --head -> exit 1"
else fail "b expected 1 for working-tree edit, got rc=$RC_B err=[$ERR_B]"; fi
git -C "$d" checkout -q -- crates/p/README.md

# ---------------------------------------------------------------- c
d="$(new_repo)"
setv "$d" 0.1.1; echo A >"$d/crates/p/README.md"; commit_all "$d" x
git -C "$d" checkout -q -b feat
echo B >"$d/crates/p/README.md"; commit_all "$d" y-samever
git -C "$d" checkout -q main
OUT="$(cd "$d" && git merge --no-edit feat 2>&1)"
if printf '%s' "$OUT" | grep -q 'VERSION COLLISION'; then pass "c fast-forward onto same-version content change -> VERSION COLLISION"
else fail "c expected VERSION COLLISION, got: [$OUT]"; fi

# ---------------------------------------------------------------- d
d="$(new_repo)"
git -C "$d" checkout -q -b L
setv "$d" 0.1.1; echo left >"$d/crates/p/README.md"; commit_all "$d" L-bump
git -C "$d" checkout -q main
git -C "$d" checkout -q -b R
setv "$d" 0.1.1; echo right >"$d/crates/p/other.md"; commit_all "$d" R-bump
git -C "$d" checkout -q L
OUT="$(cd "$d" && git merge --no-edit R 2>&1)"
MRC=$?
if [ "$MRC" -ne 0 ]; then fail "d merge itself failed (test setup should auto-merge): [$OUT]"
elif printf '%s' "$OUT" | grep -q 'VERSION COLLISION'; then pass "d true merge of two same-version bumps -> VERSION COLLISION"
else fail "d expected VERSION COLLISION, got: [$OUT]"; fi

# ---------------------------------------------------------------- e
clean_case() { # <label> <merge flags>
  local label="$1"; shift
  local d; d="$(new_repo)"
  git -C "$d" checkout -q -b feat
  setv "$d" 0.1.1; echo changed >"$d/crates/p/README.md"; commit_all "$d" bump
  git -C "$d" checkout -q main
  local out; out="$(cd "$d" && git merge --no-edit "$@" feat 2>&1)"
  local rc=$?
  if [ "$rc" -ne 0 ]; then fail "$label merge failed: [$out]"
  elif printf '%s' "$out" | grep -qE 'VERSION COLLISION|UNDETERMINED'; then fail "$label clean bump flagged: [$out]"
  else pass "$label clean bump -> no VERSION COLLISION / UNDETERMINED"; fi
}
clean_case "e1 fast-forward"
clean_case "e2 --no-ff" --no-ff

if [ "$FAILS" -ne 0 ]; then echo "$FAILS assertion(s) failed" >&2; exit 1; fi
echo "all passed"
