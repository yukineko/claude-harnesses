#!/bin/bash
# Test scripts/check-shell-syntax.py against the exact defect it was written for.
#
# Every case runs in a throwaway git repo, so nothing here depends on — or
# touches — the real tree.
#
# The load-bearing pair is A/B: A is the historical breakage reduced to its
# minimum (a lone apostrophe in a heredoc inside `$( )`, which bash 3.2 does not
# skip when scanning `$( )`), B is the SAME script with only the apostrophe
# removed. If B also failed, the gate would just be rejecting heredocs and A
# would prove nothing.
set -uo pipefail

GATE="$(cd "$(dirname "$0")/.." && pwd)/check-shell-syntax.py"
[ -f "$GATE" ] || { echo "FAIL: gate not found at $GATE" >&2; exit 1; }

fails=0
pass() { echo "  ok   — $1"; }
fail() { echo "  FAIL — $1" >&2; fails=$((fails + 1)); }
# Setup failures are not case failures: a case that runs against a fixture that
# was never built proves nothing, so abort the whole test instead of continuing.
die() { echo "ABORT: $1" >&2; exit 2; }

# Portable temp dir. The BSD "-t prefix" form (a bare name after -t, no X's) is
# BSD-only: GNU mktemp treats that argument as a template and rejects it with
# "too few X's", leaving $d empty (backlog e4a1d386). An explicit path template ending in XXXXXX means the same
# thing to both. Prints the directory, or returns non-zero with a message.
make_tmpdir() {
  tmproot="${TMPDIR:-/tmp}"; tmproot="${tmproot%/}"
  t="$(mktemp -d "$tmproot/$1.XXXXXX")" || { echo "mktemp failed for $1" >&2; return 1; }
  { [ -n "$t" ] && [ -d "$t" ]; } || { echo "mktemp returned no directory for $1: '$t'" >&2; return 1; }
  printf '%s' "$t"
}

new_repo() {
  d="$(make_tmpdir shellsyntax)" || return 1
  git -C "$d" init -q || { echo "git init failed in $d" >&2; return 1; }
  git -C "$d" config user.email t@example.com || return 1
  git -C "$d" config user.name t || return 1
  printf '%s' "$d"
}

# Writes the historical construct. $1 = dir, $2 = "bad" | "good".
# Built with printf rather than a heredoc so that this test file itself stays
# free of the very construct it is testing.
write_case() {
  d="$1"; variant="$2"
  {
    printf 'x="$(cat <<%sPY%s\n' "'" "'"
    if [ "$variant" = bad ]; then
      printf '# in_only()%ss exact-match pattern\n' "'"
    else
      printf '# the exact-match pattern in in_only()\n'
    fi
    printf 'hello\nPY\n)"\necho "$x"\n'
  } >"$d/s.sh" || die "could not write $d/s.sh"
  git -C "$d" add s.sh || die "git add failed in $d"
}

echo "check-shell-syntax.py"

# --- A: the defect is caught (this is the RED case) --------------------------
d="$(new_repo)" || die "could not create fixture repo (case A)"; write_case "$d" bad
(cd "$d" && python3 "$GATE" >/dev/null 2>&1); rc=$?
[ "$rc" -eq 1 ] && pass "unbalanced quote in a heredoc inside \$( ) is blocked (exit 1)" \
                || fail "expected exit 1 for the broken script, got $rc"

# --- B: anti-vacuity — the same script minus the apostrophe passes -----------
d="$(new_repo)" || die "could not create fixture repo (case B)"; write_case "$d" good
(cd "$d" && python3 "$GATE" >/dev/null 2>&1); rc=$?
[ "$rc" -eq 0 ] && pass "the same heredoc without the apostrophe passes (exit 0)" \
                || fail "expected exit 0 for the clean script, got $rc"

# --- C: a shebang script with no .sh suffix is still checked -----------------
d="$(new_repo)" || die "could not create fixture repo (case C)"
printf '#!/usr/bin/env bash\nif true; then\n' >"$d/hook" || die "could not write $d/hook"
git -C "$d" add hook || die "git add failed in $d"
(cd "$d" && python3 "$GATE" >/dev/null 2>&1); rc=$?
[ "$rc" -eq 1 ] && pass "an extensionless shebang script is in scope (exit 1)" \
                || fail "expected exit 1 for the extensionless broken script, got $rc"

# --- D: a repo with no shell scripts is UNDETERMINED, not clean --------------
# CLAUDE.md 3: an empty set is not a verdict. This repo has never had zero
# shell scripts, so zero means the listing failed.
d="$(new_repo)" || die "could not create fixture repo (case D)"
printf 'hi\n' >"$d/readme.md" || die "could not write $d/readme.md"
git -C "$d" add readme.md || die "git add failed in $d"
(cd "$d" && python3 "$GATE" >/dev/null 2>&1); rc=$?
[ "$rc" -eq 2 ] && pass "an empty script set resolves to undetermined (exit 2), not clean" \
                || fail "expected exit 2 for an empty script set, got $rc"

# --- E: outside a git repo it is UNDETERMINED, not clean ---------------------
d="$(make_tmpdir shellsyntax-nogit)" || die "could not create temp dir (case E)"
(cd "$d" && python3 "$GATE" >/dev/null 2>&1); rc=$?
[ "$rc" -eq 2 ] && pass "no git repo resolves to undetermined (exit 2), not clean" \
                || fail "expected exit 2 outside a git repo, got $rc"

# --- F: the real tree parses -------------------------------------------------
REPO="$(cd "$(dirname "$0")/../.." && pwd)"
# An empty $REPO would make `cd ""` a no-op and silently check the caller's cwd.
{ [ -n "$REPO" ] && [ -d "$REPO" ]; } || die "could not resolve the repository root"
(cd "$REPO" && python3 "$GATE" >/dev/null 2>&1); rc=$?
[ "$rc" -eq 0 ] && pass "this repository's tracked shell scripts all parse (exit 0)" \
                || fail "expected exit 0 for this repository, got $rc"

echo
if [ "$fails" -ne 0 ]; then
  echo "$fails case(s) failed" >&2
  exit 1
fi
echo "all cases passed"
