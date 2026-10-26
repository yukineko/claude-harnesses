#!/usr/bin/env bash
# Reproduction (backlog a44c2fd2, round 3): `condukt state check-criteria`
# extracts one pytest command from prose done_criteria and reports the
# criteria passed with skip_verifier true, while the prose names further
# test targets that the extracted command never runs.
#
# Sibling of condukt-check-criteria-multi-path.sh, which covers only
#   "python3 -m pytest scripts/test_a.py and scripts/test_b.py pass".
# This script covers the other shapes in which a second target is named:
# a backticked first command followed by a plain path, two backticked
# spans, a second full pytest command (with -k or a directory), and bare
# words ("tests", "test_b", "the tests directory") naming a target.
#
# Fixture (scratch git repo):
#   scripts/test_a.py   passes
#   scripts/test_b.py   fails
#   test_b.py           fails (root module named by the bare word test_b)
#   tests/test_c.py     fails (directory target "tests"); it also holds a
#                       failing test_smoke, selected by "pytest -k smoke"
# Controls below confirm each target really is green/red before any case.
#
# INVARIANT asserted per case (implementation-agnostic): check-criteria must
# NOT report (passed AND skip_verifier true). Every case names at least one
# failing target, so a mechanical pass with the verifier skipped is a false
# pass. Unparseable output or a missing/non-bool field is cannot-determine:
# FAIL.
#
# Positive control (separate case): "pytest scripts/test_a.py passes" names
# only a green target, so it MUST still be skip-eligible (passed true AND
# skip_verifier true). This guards against an implementation that simply
# disables the mechanical skip for every prose criteria.
#
# Drives the condukt binary (override with CONDUKT_BIN) against an isolated
# $HOME and a scratch git repo. Prints "N passed, M failed"; non-zero on any
# failure.
set -uo pipefail

CONDUKT="${CONDUKT_BIN:-condukt}"
if ! command -v "$CONDUKT" >/dev/null 2>&1; then
  echo "FAIL: condukt binary '$CONDUKT' not found (cannot determine)" >&2
  echo "0 passed, 1 failed"
  exit 2
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/condukt-cc-unacc.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

HOMEDIR="$TMP/home"
REPO="$TMP/repo"
mkdir -p "$HOMEDIR/.condukt/state" "$REPO/scripts" "$REPO/tests"
git -C "$REPO" init -q
git -C "$REPO" config user.email t@t.t
git -C "$REPO" config user.name t
printf 'def test_a_ok():\n    assert True\n' > "$REPO/scripts/test_a.py"
printf 'def test_b_fails():\n    assert False, "scripts/test_b must fail"\n' > "$REPO/scripts/test_b.py"
printf 'def test_root_b_fails():\n    assert False, "root test_b must fail"\n' > "$REPO/test_b.py"
printf 'def test_c_fails():\n    assert False, "tests/test_c must fail"\n\n\ndef test_smoke():\n    assert False, "smoke must fail"\n' > "$REPO/tests/test_c.py"
git -C "$REPO" add scripts/test_a.py scripts/test_b.py test_b.py tests/test_c.py
git -C "$REPO" commit -q -m init

control_fail() {
  echo "FAIL: control: $1 (cannot determine)" >&2
  echo "0 passed, 1 failed"
  exit 2
}
pyt() { (cd "$REPO" && python3 -m pytest -q -p no:cacheprovider "$@" >/dev/null 2>&1); }

# Controls: each target really has the colour the cases rely on.
pyt scripts/test_a.py || control_fail "scripts/test_a.py does not pass on its own"
pyt scripts/test_b.py && control_fail "scripts/test_b.py unexpectedly passes"
pyt test_b.py && control_fail "root test_b.py unexpectedly passes"
pyt tests && control_fail "tests directory unexpectedly passes"
pyt -k smoke && control_fail "pytest -k smoke unexpectedly passes"
pyt scripts/test_a.py tests && control_fail "scripts/test_a.py plus tests unexpectedly passes"
pyt scripts/test_a.py test_b.py && control_fail "scripts/test_a.py plus test_b.py unexpectedly passes"

run_condukt() {
  (cd "$REPO" && HOME="$HOMEDIR" CONDUKT_AUTONOMOUS=0 "$CONDUKT" "$@" </dev/null)
}

# Prints OK/VIOLATION/UNDETERMINED line for one done_criteria string.
# $1 = case label, $2 = done_criteria text.
classify() {
  local label="$1" criteria="$2" decomp init_out rid cc_out cc_rc
  decomp="$TMP/decomp-$label.json"
  CRITERIA="$criteria" python3 -c '
import json, os, sys
d = {"goal": "g", "tasks": [{"id": "t1", "title": "add tests",
     "touched_files": ["scripts/test_a.py", "scripts/test_b.py", "test_b.py", "tests/test_c.py"],
     "deps": [], "class": "serial", "done_criteria": os.environ["CRITERIA"]}]}
json.dump(d, open(sys.argv[1], "w"))
' "$decomp" || { echo "UNDETERMINED could not write decomposition"; return; }
  init_out="$(run_condukt state init --file "$decomp" 2>&1)"
  rid="$(printf '%s\n' "$init_out" | grep -E '^run-' | tail -1 | tr -d '[:space:]')"
  if [ -z "$rid" ]; then
    echo "UNDETERMINED state init produced no run id: $(printf '%s' "$init_out" | tr '\n' ' ')"
    return
  fi
  cc_out="$(run_condukt state check-criteria --run "$rid" --task t1 2>"$TMP/cc-$label.err")"
  cc_rc=$?
  printf '%s' "$cc_out" | CC_RC="$cc_rc" python3 -c '
import json, os, sys
rc = os.environ.get("CC_RC")
try:
    d = json.loads(sys.stdin.read())
except Exception as e:
    print("UNDETERMINED exit=%s unparseable output: %s" % (rc, e)); sys.exit(0)
if not isinstance(d, dict):
    print("UNDETERMINED exit=%s output is not a JSON object" % rc); sys.exit(0)
passed = d.get("passed")
skip = d.get("skip_verifier")
if not isinstance(passed, bool) or not isinstance(skip, bool):
    print("UNDETERMINED exit=%s passed=%r skip_verifier=%r (missing or non-bool)" % (rc, passed, skip)); sys.exit(0)
tag = "VIOLATION" if (passed and skip) else "OK"
print("%s exit=%s passed=%r skip_verifier=%r cmd=%s" % (tag, rc, passed, skip, json.dumps(d.get("cmd"))))
'
}

PASS=0
FAIL=0

# Violation cases: each must NOT be (passed AND skip_verifier).
check_not_skipped() {
  local label="$1" criteria="$2" v
  v="$(classify "$label" "$criteria")"
  case "$v" in
    OK*)
      echo "PASS [$label] not a mechanical pass with verifier skipped: $v"
      PASS=$((PASS + 1)) ;;
    VIOLATION*)
      echo "FAIL [$label] reported passed with skip_verifier true although a named target fails: $v (criteria: $criteria)"
      FAIL=$((FAIL + 1)) ;;
    *)
      echo "FAIL [$label] cannot determine: $v (criteria: $criteria)"
      FAIL=$((FAIL + 1)) ;;
  esac
}

# Positive control: must be (passed AND skip_verifier).
check_skip_eligible() {
  local label="$1" criteria="$2" v
  v="$(classify "$label" "$criteria")"
  case "$v" in
    VIOLATION*)
      echo "PASS [$label] single green target is still skip-eligible: $v"
      PASS=$((PASS + 1)) ;;
    OK*)
      echo "FAIL [$label] single green target is no longer skip-eligible (skip disabled wholesale?): $v (criteria: $criteria)"
      FAIL=$((FAIL + 1)) ;;
    *)
      echo "FAIL [$label] cannot determine: $v (criteria: $criteria)"
      FAIL=$((FAIL + 1)) ;;
  esac
}

check_not_skipped 1a '`python3 -m pytest scripts/test_a.py` and scripts/test_b.py pass'
check_not_skipped 1b '`pytest scripts/test_a.py` and `scripts/test_b.py` pass'
check_not_skipped 2a 'pytest scripts/test_a.py passes and pytest -k smoke passes'
check_not_skipped 2b 'pytest scripts/test_a.py passes and python3 -m pytest tests passes'
check_not_skipped 3a 'pytest scripts/test_a.py and tests pass'
check_not_skipped 3b 'pytest scripts/test_a.py and test_b pass'
check_not_skipped 3c 'pytest scripts/test_a.py and the tests directory pass'
check_skip_eligible pos 'pytest scripts/test_a.py passes'

echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
