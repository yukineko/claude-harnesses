#!/usr/bin/env bash
# Reproduction (backlog a44c2fd2): `condukt state check-criteria` runs only the
# first of several pytest paths named in prose done_criteria, yet reports the
# criteria as passed with skip_verifier true.
#
# Commit 4525177e added `prose_pytest_argv` (crates/condukt/src/verify.rs) so
# the prose fallback of `verify::mechanical_cmd` keeps "python3 -m pytest
# PATH". Its token loop stops at the first plain prose word, so for
#   "python3 -m pytest scripts/test_a.py and scripts/test_b.py pass"
# it yields ["python3","-m","pytest","scripts/test_a.py"]. Coverage stays
# Single, check-criteria reports passed with skip_verifier true, and a
# failing scripts/test_b.py is never run: a criteria the task does NOT meet
# is recorded as mechanically met and the verifier is skipped.
#
# INVARIANT asserted (implementation-agnostic): with test_a passing and
# test_b failing, check-criteria must NOT report (passed AND skip_verifier
# true). Acceptable outcomes: every named path is run (so the check fails on
# test_b), or coverage degrades to partial/undecidable so skip_verifier is
# false. Unparseable output or a missing field is cannot-determine: FAIL.
#
# Drives the condukt binary (override with CONDUKT_BIN) against an isolated
# $HOME and a scratch git repo. Prints "N passed, M failed"; non-zero on fail.
set -uo pipefail

CONDUKT="${CONDUKT_BIN:-condukt}"
if ! command -v "$CONDUKT" >/dev/null 2>&1; then
  echo "FAIL: condukt binary '$CONDUKT' not found (cannot determine)" >&2
  echo "0 passed, 1 failed"
  exit 2
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/condukt-cc-multi.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

HOMEDIR="$TMP/home"
REPO="$TMP/repo"
mkdir -p "$HOMEDIR/.condukt/state" "$REPO/scripts"
git -C "$REPO" init -q
git -C "$REPO" config user.email t@t.t
git -C "$REPO" config user.name t
printf 'def test_a_ok():\n    assert True\n' > "$REPO/scripts/test_a.py"
printf 'def test_b_fails():\n    assert False, "test_b must fail"\n' > "$REPO/scripts/test_b.py"
git -C "$REPO" add scripts/test_a.py scripts/test_b.py
git -C "$REPO" commit -q -m init

# Control: the fixture really is (test_a green, test_b red). Without this a
# broken pytest setup would make the invariant check meaningless.
if ! (cd "$REPO" && python3 -m pytest -q scripts/test_a.py >/dev/null 2>&1); then
  echo "FAIL: control: scripts/test_a.py does not pass on its own (cannot determine)" >&2
  echo "0 passed, 1 failed"
  exit 2
fi
if (cd "$REPO" && python3 -m pytest -q scripts/test_b.py >/dev/null 2>&1); then
  echo "FAIL: control: scripts/test_b.py unexpectedly passes (cannot determine)" >&2
  echo "0 passed, 1 failed"
  exit 2
fi

cat > "$REPO/decomp.json" <<'JSON'
{"goal":"g","tasks":[{"id":"t1","title":"add two tests","touched_files":["scripts/test_a.py","scripts/test_b.py"],"deps":[],"class":"serial","done_criteria":"python3 -m pytest scripts/test_a.py and scripts/test_b.py pass"}]}
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
cc_rc=$?
echo "check-criteria exit: $cc_rc"
echo "check-criteria stdout: $cc_out"

verdict="$(printf '%s' "$cc_out" | python3 -c '
import json, sys
try:
    d = json.loads(sys.stdin.read())
except Exception as e:
    print("UNDETERMINED unparseable output: %s" % e); sys.exit(0)
if not isinstance(d, dict):
    print("UNDETERMINED output is not a JSON object"); sys.exit(0)
passed = d.get("passed")
skip = d.get("skip_verifier")
if not isinstance(passed, bool) or not isinstance(skip, bool):
    print("UNDETERMINED passed=%r skip_verifier=%r (missing or non-bool)" % (passed, skip)); sys.exit(0)
if passed and skip:
    print("VIOLATION passed=True skip_verifier=True cmd=%s" % json.dumps(d.get("cmd")))
else:
    print("OK passed=%r skip_verifier=%r cmd=%s" % (passed, skip, json.dumps(d.get("cmd"))))
')"
echo "verdict: $verdict"

case "$verdict" in
  OK*)
    echo "PASS: multi-path pytest criteria is not reported as mechanically passed with the verifier skipped"
    echo "1 passed, 0 failed"
    exit 0
    ;;
  VIOLATION*)
    echo "FAIL: check-criteria reported passed with skip_verifier true although scripts/test_b.py fails ($verdict)" >&2
    echo "0 passed, 1 failed"
    exit 1
    ;;
  *)
    echo "FAIL: cannot determine check-criteria outcome ($verdict)" >&2
    echo "0 passed, 1 failed"
    exit 1
    ;;
esac
