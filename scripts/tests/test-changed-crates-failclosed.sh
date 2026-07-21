#!/usr/bin/env bash
# Self-contained test for the FAIL-CLOSED handling in scripts/test-changed-crates.sh
# (ae027186). The script derives the changed-crate set from `git diff --name-only
# HEAD` + `git ls-files --others`. Historically both were piped through
# `2>/dev/null` into a string, so a FAILED git invocation collapsed to an empty
# change set, read as "no crate touched -> exit 0", and silently passed the turn
# having tested nothing — the twin of the already-fail-closed cargo-absent branch.
#
# This proves:
#   A. genuine clean (git succeeds, nothing changed) still exits 0 (no regression);
#   B. a git-diff FAILURE now fails CLOSED (exit 1), not a silent pass;
#   C. an UNBORN branch (no commits) is handled as determinable ("everything
#      present is new"), not fail-closed — the staged file is still discovered.
#
# MAC-RUNNABLE: pure git + bash in a temp repo sandbox; no GNU coreutils, no
# network, no real ~/.claude touched.
#
# Exit 0 on success, non-zero on any failed assertion.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$REPO/scripts/test-changed-crates.sh"
REAL_GIT="$(command -v git)"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/test-changed-failclosed.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

git_init() { # <dir>
  "$REAL_GIT" -C "$1" init -q
  "$REAL_GIT" -C "$1" config user.email t@t.t
  "$REAL_GIT" -C "$1" config user.name t
}

# =============================================================================
# Part A: genuine clean — git succeeds, nothing changed -> exit 0 (no regression)
# =============================================================================
echo ">>> A: clean repo, nothing changed — expect exit 0 (nothing to test)"
A="$TMP/a"; mkdir -p "$A"; git_init "$A"
echo "hello" >"$A/README.md"
"$REAL_GIT" -C "$A" add README.md >/dev/null
"$REAL_GIT" -C "$A" commit -qm init
OUT="$( cd "$A" && bash "$SCRIPT" 2>&1; echo "RC=$?" )"
RC="$(grep -o 'RC=[0-9]*$' <<<"$OUT" | tail -1 | cut -d= -f2)"
echo "$OUT" | sed 's/^/    /'
[ "$RC" -eq 0 ] || fail "clean repo should exit 0, got $RC"
grep -q "no crate touched" <<<"$OUT" || fail "clean repo should report 'no crate touched'"
pass "clean repo exits 0 (genuine-empty still passes)"

# =============================================================================
# Part B: git diff FAILS -> fail CLOSED (exit 1), not a silent pass.
# A git stub fails only `diff`; every other subcommand delegates to real git, so
# `rev-parse --show-toplevel` and `rev-parse --verify HEAD` still work (HEAD
# exists here, so the unborn fallback does NOT fire — the failure is genuine).
# =============================================================================
echo
echo ">>> B: git diff fails (real HEAD exists) — expect fail-closed (exit 1)"
STUBDIR="$TMP/stubbin"; mkdir -p "$STUBDIR"
cat >"$STUBDIR/git" <<STUBEOF
#!/usr/bin/env bash
if [ "\$1" = "diff" ]; then
  echo "fatal: simulated git diff failure" >&2
  exit 128
fi
exec "$REAL_GIT" "\$@"
STUBEOF
chmod +x "$STUBDIR/git"
OUT="$( cd "$A" && PATH="$STUBDIR:$PATH" bash "$SCRIPT" 2>&1; echo "RC=$?" )"
RC="$(grep -o 'RC=[0-9]*$' <<<"$OUT" | tail -1 | cut -d= -f2)"
echo "$OUT" | sed 's/^/    /'
[ "$RC" -eq 1 ] || fail "a git-diff failure must fail closed (exit 1), got $RC"
pass "git-diff failure exits 1 (fail-closed)"
grep -q "could not determine the changed file set" <<<"$OUT" || fail "did not report the cannot-determine reason"
pass "reported the cannot-determine reason"
grep -q "no crate touched" <<<"$OUT" && fail "STILL reports 'nothing to test' on a git failure — fail-open not closed"
pass "did NOT report 'nothing to test' on a git failure (fail-open closed)"

# =============================================================================
# Part C: UNBORN branch (no commits) with a staged non-crate-manifest file —
# handled as determinable (not fail-closed); the staged file IS discovered.
# =============================================================================
echo
echo ">>> C: unborn branch (no HEAD), staged crates/skillonly/SKILL.md — expect exit 0, discovered"
C="$TMP/c"; mkdir -p "$C"; git_init "$C"
mkdir -p "$C/crates/skillonly"
echo "# skill" >"$C/crates/skillonly/SKILL.md"
"$REAL_GIT" -C "$C" add crates/skillonly/SKILL.md >/dev/null   # staged, so it shows via `git ls-files`
OUT="$( cd "$C" && bash "$SCRIPT" 2>&1; echo "RC=$?" )"
RC="$(grep -o 'RC=[0-9]*$' <<<"$OUT" | tail -1 | cut -d= -f2)"
echo "$OUT" | sed 's/^/    /'
[ "$RC" -eq 0 ] || fail "unborn branch (benign, determinable) must not fail closed; got $RC"
pass "unborn branch did NOT fail closed (exit 0)"
# "skill-only" (no Cargo.toml) proves the unborn fallback POPULATED the change
# set from `git ls-files` — otherwise dirs would be empty -> "no crate touched".
grep -q "no Cargo.toml (skill-only" <<<"$OUT" || fail "unborn fallback did not discover the staged crates/ file (got 'no crate touched'?)"
pass "unborn fallback discovered the staged crates/ path (determinable, not fail-closed)"

echo
echo "PASS: a git-diff failure now fails CLOSED (exit 1) instead of silently"
echo "      passing as 'nothing to test'; genuine-clean and unborn-HEAD states"
echo "      are correctly distinguished from a cannot-determine failure."
