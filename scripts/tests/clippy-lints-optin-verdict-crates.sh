#!/usr/bin/env bash
# scripts/tests/clippy-lints-optin-verdict-crates.sh
#
# Pins backlog 034b6620 for the REAL crates `tdd` (crates/tdd) and `condukt`
# (crates/condukt): both must carry `[lints] workspace = true`, and
# scripts/check-clippy-lints.py must refuse a working-tree change that drops it.
#
# Why this is a SEPARATE test from scripts/tests/precommit-clippy-gate.sh
# ------------------------------------------------------------------------
# That suite proves the mechanism on synthetic fixture crates. It cannot tell
# you whether any particular real crate in THIS repository has opted in. tdd
# and condukt are verdict-bearing gate crates, so an unwrap()-driven panic in
# either is the "cannot-determine turned into a crash" case CLAUDE.md 3 names.
#
# check-clippy-lints.py no longer runs cargo clippy (donegate's lint-changed
# check owns that). It scans [lints] CONFIGURATION only. So the questions this
# test answers, as observations on a sandbox copy of the REAL working tree, are:
#
#   opted-in  — the real crates/<crate>/Cargo.toml has `[lints] workspace = true`
#               (read with tomllib, not grepped).
#   dropped   — removing that opt-in from the real manifest makes the scanner
#               block (exit 1) and name crates/<crate>/Cargo.toml.
#   control   — anti-vacuity: from a FRESH reset, a harmless edit to the same
#               manifest (a trailing comment) exits 0. Without it, a scanner that
#               always blocks would satisfy "dropped".
#
# Sandbox construction — copy of the WORKING TREE, not HEAD
# -----------------------------------------------------------
# `git ls-files -z -co --exclude-standard` enumerates what is on disk now
# (tracked + untracked-not-ignored), mirroring the scanner's own working-tree
# selection. `target/` is excluded. The copy is `git init` + committed inside
# the sandbox so the scanner has a HEAD to diff against. This script NEVER
# mutates the real repository; every destructive git command targets $SANDBOX
# under a fresh mktemp -d, cleaned up by the EXIT trap.
#
# MAC-RUNNABLE: bash 3.2, BSD tools, python3, git. No cargo, no network.
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
echo "using repo:    $REPO"
echo "using scanner: $SCANNER"

TMP="$(mktemp -d "${TMPDIR:-/tmp}/clippy-lints-optin-verdict.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

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
[ -f "$SANDBOX/crates/tdd/Cargo.toml" ] || fail "sandbox copy is missing crates/tdd/Cargo.toml"
[ -f "$SANDBOX/crates/condukt/Cargo.toml" ] || fail "sandbox copy is missing crates/condukt/Cargo.toml"
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

run_cases() { # <crate>
  local crate="$1" rel="crates/$1/Cargo.toml"

  echo
  echo ">>> $crate / opted-in: $rel has [lints] workspace = true"
  reset_sandbox
  if "$PY" - "$SANDBOX/$rel" <<'PYEOF'
import sys, tomllib
with open(sys.argv[1], "rb") as f:
    m = tomllib.load(f)
lints = m.get("lints")
sys.exit(0 if isinstance(lints, dict) and lints.get("workspace") is True else 1)
PYEOF
  then
    pass "$crate/opted-in: [lints] workspace = true present"
  else
    note_fail "$crate/opted-in: $rel does not carry [lints] workspace = true"
  fi

  echo
  echo ">>> $crate / dropped: opt-in removed from $rel — expect exit 1"
  reset_sandbox
  # Drop the `workspace = true` line that follows `[lints]` (and the header).
  "$PY" - "$SANDBOX/$rel" <<'PYEOF'
import sys
p = sys.argv[1]
lines = open(p).read().splitlines(keepends=True)
out, in_lints = [], False
for line in lines:
    s = line.strip()
    if s.startswith("["):
        in_lints = s == "[lints]"
        if in_lints:
            continue
    if in_lints and s.replace(" ", "") == "workspace=true":
        continue
    out.append(line)
open(p, "w").write("".join(out))
PYEOF
  if "$REAL_GIT" -C "$SANDBOX" diff --quiet HEAD -- "$rel"; then
    note_fail "$crate/dropped: fixture is wrong — $rel did not change"
  fi
  scan
  if [ "$RC" != "1" ]; then
    note_fail "$crate/dropped: expected exit 1 (blocked), got $RC"
  else
    pass "$crate/dropped: exit 1"
  fi
  if printf '%s\n' "$OUT" | grep -q "$rel"; then
    pass "$crate/dropped: the crate manifest is named"
  else
    note_fail "$crate/dropped: $rel was not named in the block reason"
  fi

  echo
  echo ">>> $crate / control (anti-vacuity): harmless edit to $rel — expect exit 0"
  reset_sandbox
  printf '%s\n' '# harmless trailing comment (034b6620 control)' >>"$SANDBOX/$rel"
  scan
  if [ "$RC" != "0" ]; then
    note_fail "$crate/control: expected exit 0 (clean), got $RC"
  else
    pass "$crate/control: exit 0"
  fi
}

for crate in tdd condukt; do
  run_cases "$crate"
done

echo
if [ "$OVERALL_FAIL" != "0" ]; then
  echo "FAIL: tdd and/or condukt are not (fully) protected by the [lints]"
  echo "      opt-in scan. See the FAIL lines above."
  exit 1
fi

echo "PASS: tdd and condukt both carry [lints] workspace = true, dropping it"
echo "      blocks (exit 1), and a harmless manifest edit passes (exit 0)."
