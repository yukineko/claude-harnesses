#!/usr/bin/env bash
# Self-contained test for the markdown-only carve-out in
# scripts/check-version-bumped.py.
#
# Proves:
#   A. a change confined to *.md files (README.md, skills/x/SKILL.md) demands
#      no bump (exit 0) and ANNOUNCES the exemption;
#   B. a source-only change with no bump STILL fails (exit 1);
#   C. *.md plus a source file with no bump STILL fails (exit 1);
#   D. a .md-only change WITH a bump still passes (exit 0);
#   E. *.md plus .claude-plugin/plugin.json (non-.md) with no version move
#      STILL fails (exit 1).
#
# MAC-RUNNABLE: pure git + python3 in a temp repo sandbox.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$REPO/scripts/check-version-bumped.py"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

TMP="$(mktemp -d "${TMPDIR:-/tmp}/version-bumped-md-only.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

SANDBOX="$TMP/repo"
mkdir -p "$SANDBOX/crates/foo/.claude-plugin" "$SANDBOX/crates/foo/src" "$SANDBOX/crates/foo/skills/x"
git -C "$SANDBOX" init -q
git -C "$SANDBOX" config user.email t@t.t
git -C "$SANDBOX" config user.name t

PJ="$SANDBOX/crates/foo/.claude-plugin/plugin.json"
write_version() { # <version> [description]
  python3 -c 'import json,sys; open(sys.argv[1],"w").write(json.dumps({"name":"foo","version":sys.argv[2],"description":sys.argv[3]})+"\n")' "$PJ" "$1" "${2:-d}"
}
put() { # <path-under-crates/foo> <content>
  python3 -c 'import sys; open(sys.argv[1],"w").write(sys.argv[2]+"\n")' "$SANDBOX/crates/foo/$1" "$2"
}

write_version 1.0.0
put README.md "readme v1"
put skills/x/SKILL.md "skill v1"
put src/main.rs "fn main() {}"
git -C "$SANDBOX" add -A
git -C "$SANDBOX" commit -qm base

reset_tree() { git -C "$SANDBOX" checkout -q -- .; }

run_gate() {
  OUT="$(cd "$SANDBOX" && python3 "$SCRIPT" 2>&1)"
  RC=$?
}

# A
put README.md "readme v2"
put skills/x/SKILL.md "skill v2"
run_gate
[ "$RC" -eq 0 ] || fail "A: a .md-only change must not demand a bump; rc=$RC out=$OUT"
case "$OUT" in
  *"ONLY *.md"*) : ;;
  *) fail "A: the carve-out must announce itself; out=$OUT" ;;
esac
pass "A: .md-only change passes and the exemption is announced"
reset_tree

# B
put src/main.rs "fn main() { let _ = 1; }"
run_gate
[ "$RC" -eq 1 ] || fail "B: source change with no bump must fail; rc=$RC out=$OUT"
pass "B: source-only change still blocks"
reset_tree

# C
put README.md "readme v3"
put src/main.rs "fn main() { let _ = 2; }"
run_gate
[ "$RC" -eq 1 ] || fail "C: .md + source with no bump must fail; rc=$RC out=$OUT"
pass "C: .md plus source still blocks"
reset_tree

# D
put README.md "readme v4"
write_version 1.0.1
run_gate
[ "$RC" -eq 0 ] || fail "D: bumped .md-only change must pass; rc=$RC out=$OUT"
pass "D: .md-only change with a bump passes"
reset_tree

# E
put README.md "readme v5"
write_version 1.0.0 changed-desc
run_gate
[ "$RC" -eq 1 ] || fail "E: .md + plugin.json edit with no bump must fail; rc=$RC out=$OUT"
pass "E: .md plus plugin.json still blocks"
reset_tree

echo "ALL PASS: version-bumped-md-only.sh"
