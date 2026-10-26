#!/usr/bin/env bash
# Reproduction: `condukt state check-criteria` drops the `python3 -m` prefix.
#
# Observed 2026-10-26 on run-20261026-025716-14405 task t3 (condukt 0.7.295):
# the done_criteria said "python3 -m pytest scripts/test_backlog_audit_b1_0.py
# -k main_guard passes" in plain prose (no backticks). The prose fallback of
# `verify::mechanical_cmd` (crates/condukt/src/verify.rs) matches the substring
# "pytest" and returns the bare argv ["pytest"]. Bare pytest does not put cwd
# on sys.path and has no file argument, so it collected the whole worktree and
# failed on scripts/test_backlog_5461ba1c.py with
# "ModuleNotFoundError: No module named 'scripts'" — a criteria the task met
# was run as a different command than the one it named.
#
# INVARIANT asserted: for a done_criteria naming
# "python3 -m pytest scripts/x.py", the command check-criteria extracts (and
# runs) starts with `python3 -m pytest` and keeps the `scripts/x.py` target.
#
# The test drives the INSTALLED condukt binary (override with CONDUKT_BIN)
# against an isolated $HOME and a scratch git repo, so no real run state is
# touched. Prints "<N> passed" / "<N> failed" and exits non-zero on failure.
set -uo pipefail

CONDUKT="${CONDUKT_BIN:-condukt}"
if ! command -v "$CONDUKT" >/dev/null 2>&1; then
  echo "FAIL: condukt binary '$CONDUKT' not found (cannot determine)" >&2
  echo "0 passed, 1 failed"
  exit 2
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/condukt-cc-pym.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

HOMEDIR="$TMP/home"
REPO="$TMP/repo"
mkdir -p "$HOMEDIR/.condukt/state" "$REPO/scripts"
git -C "$REPO" init -q
git -C "$REPO" config user.email t@t.t
git -C "$REPO" config user.name t
printf 'def test_ok():\n    assert True\n' > "$REPO/scripts/x.py"
git -C "$REPO" add scripts/x.py
git -C "$REPO" commit -q -m init

cat > "$REPO/decomp.json" <<'JSON'
{"goal":"g","tasks":[{"id":"t1","title":"add a test","touched_files":["scripts/x.py"],"deps":[],"class":"serial","done_criteria":"the new test exists and python3 -m pytest scripts/x.py passes"}]}
JSON

run_condukt() {
  (cd "$REPO" && HOME="$HOMEDIR" CONDUKT_AUTONOMOUS=0 "$CONDUKT" "$@" </dev/null)
}

init_out="$(run_condukt state init --file "$REPO/decomp.json" 2>&1)"
rid="$(printf '%s\n' "$init_out" | grep -E '^run-' | tail -1 | tr -d '[:space:]')"
if [ -z "$rid" ]; then
  echo "FAIL: state init produced no run id (cannot determine): $init_out" >&2
  echo "0 passed, 1 failed"
  exit 2
fi

cc_out="$(run_condukt state check-criteria --run "$rid" --task t1 2>"$TMP/cc.err")"
echo "check-criteria stdout: $cc_out"

argv="$(printf '%s' "$cc_out" | python3 -c '
import json, sys
try:
    d = json.loads(sys.stdin.read())
except Exception as e:
    print("UNPARSEABLE:%s" % e); sys.exit(0)
cmd = d.get("cmd")
if cmd is None and isinstance(d.get("evidence"), dict):
    cmd = d["evidence"].get("cmd")
print(json.dumps(cmd))
')"
echo "extracted argv: $argv"

if printf '%s' "$argv" | python3 -c '
import json, sys
a = json.loads(sys.stdin.read())
ok = isinstance(a, list) and a[:3] == ["python3", "-m", "pytest"] and "scripts/x.py" in a
sys.exit(0 if ok else 1)
' 2>/dev/null; then
  echo "PASS: extracted command keeps the python3 -m prefix and target"
  echo "1 passed, 0 failed"
  exit 0
else
  echo "FAIL: extracted command is $argv; expected it to start with [\"python3\",\"-m\",\"pytest\"] and include scripts/x.py" >&2
  echo "0 passed, 1 failed"
  exit 1
fi
