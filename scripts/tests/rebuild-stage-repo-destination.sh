#!/usr/bin/env bash
# Regression test: `rebuild-plugins.sh --stage-repo` must never resolve its copy
# DESTINATION to a file outside `crates/*/bin/`.
#
# The incident (2026-08-21, measured on this repo). The destination was resolved
# with
#
#     repofile=$(ls "$REPO"/crates/*/bin/"$base" 2>/dev/null | head -n1 || true)
#
# under `shopt -s nullglob`. Only 2 of 39 plugins actually had a staged
# `crates/<name>/bin/<name>-linux-x86_64`, so for the other 37 the glob expanded
# to NOTHING, `ls` was left with ZERO arguments, and it therefore listed `$PWD`
# — the repo root — instead of failing. `head -n1` took the first entry in
# C-locale order, `CLAUDE.md`, and the script then ran
#
#     cp -f "$src" CLAUDE.md
#
# 38 times, overwriting the repo's CLAUDE.md with a plugin ELF binary (recovered
# from git; `git status` clean afterwards). `2>/dev/null` hid nothing (ls
# SUCCEEDED) and `|| true` was inert.
#
# This is CLAUDE.md §3 in its most literal form: "there is no staged copy" — a
# resolution FAILURE — was silently rewritten into a plausible-looking answer,
# "the staged copy is ./CLAUDE.md", and that answer was then used as a
# destructive write target. The fix must leave `repofile` EMPTY on a miss so the
# existing `[ -n "$repofile" ]` guard skips the copy.
#
# Runs entirely under --dry-run against a temp cache. Nothing real is written.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
REBUILD="$REPO/scripts/rebuild-plugins.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/rebuild-stage-dest.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

# The loop keys off `target/release/<binname>` existing, so pick a real
# workspace binary and make sure it is built. `backlog` is chosen because
# `crates/backlog/bin/` holds only the launcher — no staged
# `backlog-linux-x86_64` — which is exactly the miss that triggered the bug.
BINNAME=backlog
REL="$REPO/target/release/$BINNAME"
if [ ! -x "$REL" ]; then
  echo "  (building $BINNAME --release for the fixture)"
  (cd "$REPO" && cargo build --release -p "$BINNAME" --quiet) \
    || fail "could not build the $BINNAME fixture binary"
fi
[ -x "$REL" ] || fail "$REL still missing after build"

# A staged copy must NOT exist for this name — that absence IS the fixture.
if compgen -G "$REPO/crates/*/bin/$BINNAME-linux-x86_64" >/dev/null; then
  fail "fixture invalid: a staged crates/*/bin/$BINNAME-linux-x86_64 exists, so \
this run cannot exercise the glob-miss path. Pick a plugin with no staged binary."
fi

# Temp cache holding one plugin whose per-platform binary name is $BINNAME, so
# the refresh loop reaches the --stage-repo block for it.
TEST_CACHE="$TMP/cache/yukineko"
mkdir -p "$TEST_CACHE/$BINNAME/0.0.1/bin"
: >"$TEST_CACHE/$BINNAME/0.0.1/bin/$BINNAME-linux-x86_64"
chmod +x "$TEST_CACHE/$BINNAME/0.0.1/bin/$BINNAME-linux-x86_64"

OUT="$(cd "$REPO" && CLAUDE_PLUGIN_CACHE="$TEST_CACHE" \
  bash "$REBUILD" --no-clean --dry-run --stage-repo 2>&1)"

# 1) The exact observed damage: never name a repo-root file as the destination.
if grep -qE '^repo +would update CLAUDE\.md$' <<<"$OUT"; then
  echo "$OUT" | grep -E '^repo +would update' | head -3 >&2
  fail "--stage-repo resolved its destination to the repo-root CLAUDE.md"
fi
pass "no repo-root CLAUDE.md destination"

# 2) The general property, so the next miss cannot pick a different victim:
#    every staged destination lives under crates/<name>/bin/.
BAD="$(grep -E '^repo +would update' <<<"$OUT" \
  | sed -E 's/^repo +would update //' \
  | grep -vE '^crates/[^/]+/bin/' || true)"
[ -z "$BAD" ] || {
  echo "$BAD" >&2
  fail "--stage-repo named destination(s) outside crates/*/bin/"
}
pass "every --stage-repo destination is under crates/*/bin/"

# 3) Anti-vacuity control. The two assertions above also pass if the loop never
#    reached the --stage-repo block at all — then they prove nothing. Require
#    evidence that this run really did consider the fixture plugin.
grep -qE "cache bins scanned: [1-9]" <<<"$OUT" \
  || { echo "$OUT" | tail -5 >&2; fail "the refresh loop scanned no cache binary — assertions above are vacuous"; }
pass "the refresh loop did scan the fixture (assertions are not vacuous)"

echo "PASS: rebuild-plugins.sh --stage-repo destination stays inside crates/*/bin/"
