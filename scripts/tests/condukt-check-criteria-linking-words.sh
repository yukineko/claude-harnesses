#!/usr/bin/env bash
# Reproduction (backlog a44c2fd2): `condukt state check-criteria` extracts a
# mechanical pytest command from prose done_criteria and reports
# passed=true + skip_verifier=true although a test target named in the prose
# was never run (or fails). This script covers the "linking word" shapes
# that hide a second target: "plus", "as well as", comma juxtaposition,
# "then", Japanese connectives, a flag value followed by "and <path>", and
# a -k expression naming a failing test.
#
# Sibling of condukt-check-criteria-unaccounted-targets.sh (same fixture
# style, same classify invariant).
#
# Fixture (scratch git repo):
#   scripts/test_a.py   passes
#   scripts/test_b.py   fails
#   scripts/test_k.py   holds test_ok (passes) and test_bad (fails). It is a
#                       SEPARATE file so test_a.py stays green for every
#                       other case; case 8 selects it with -k.
#
# INVARIANT per violation case: must NOT report (passed AND skip_verifier).
# Positive controls (case 9): single green target stays skip-eligible.
# Degenerate cases (case 10): binary must not panic (exit 101 / "panicked")
# and must not report passed+skip_verifier.
#
# Drives the condukt binary (CONDUKT_BIN) against an isolated $HOME and a
# scratch git repo. Prints "N passed, M failed"; non-zero on any failure.
set -uo pipefail

CONDUKT="${CONDUKT_BIN:-condukt}"
if ! command -v "$CONDUKT" >/dev/null 2>&1; then
  echo "FAIL: condukt binary '$CONDUKT' not found (cannot determine)" >&2
  echo "0 passed, 1 failed"
  exit 2
fi

TMP="$(mktemp -d "${TMPDIR:-/tmp}/condukt-cc-link.XXXXXX")"
cleanup() { rm -rf "$TMP"; }
trap cleanup EXIT

HOMEDIR="$TMP/home"
REPO="$TMP/repo"
mkdir -p "$HOMEDIR/.condukt/state" "$REPO/scripts"
git -C "$REPO" init -q
git -C "$REPO" config user.email t@t.t
git -C "$REPO" config user.name t
printf 'def test_a_ok():\n    assert True\n' > "$REPO/scripts/test_a.py"
printf 'def test_b_fails():\n    assert False, "scripts/test_b must fail"\n' > "$REPO/scripts/test_b.py"
printf 'def test_ok():\n    assert True\n\n\ndef test_bad():\n    assert False, "test_bad must fail"\n' > "$REPO/scripts/test_k.py"
git -C "$REPO" add scripts/test_a.py scripts/test_b.py scripts/test_k.py
git -C "$REPO" commit -q -m init

control_fail() {
  echo "FAIL: control: $1 (cannot determine)" >&2
  echo "0 passed, 1 failed"
  exit 2
}
pyt() { (cd "$REPO" && python3 -m pytest -q -p no:cacheprovider "$@" >/dev/null 2>&1); }

pyt scripts/test_a.py || control_fail "scripts/test_a.py does not pass on its own"
pyt scripts/test_b.py && control_fail "scripts/test_b.py unexpectedly passes"
pyt scripts/test_k.py -k "test_ok or test_bad" && control_fail "test_k.py with test_bad unexpectedly passes"
pyt scripts/test_k.py -k test_ok || control_fail "test_k.py -k test_ok does not pass"

run_condukt() {
  (cd "$REPO" && HOME="$HOMEDIR" CONDUKT_AUTONOMOUS=0 "$CONDUKT" "$@" </dev/null)
}

# Prints one verdict line for one done_criteria string:
#   OK|VIOLATION|PANIC|REJECTED|UNDETERMINED ...
# REJECTED = state init refused the criteria (no run id), without panic.
# $1 = label, $2 = criteria.
classify() {
  local label="$1" criteria="$2" decomp init_out init_rc rid cc_out cc_rc errf
  decomp="$TMP/decomp-$label.json"
  errf="$TMP/cc-$label.err"
  CRITERIA="$criteria" python3 -c '
import json, os, sys
d = {"goal": "g", "tasks": [{"id": "t1", "title": "add tests",
     "touched_files": ["scripts/test_a.py", "scripts/test_b.py", "scripts/test_k.py"],
     "deps": [], "class": "serial", "done_criteria": os.environ["CRITERIA"]}]}
json.dump(d, open(sys.argv[1], "w"))
' "$decomp" || { echo "UNDETERMINED could not write decomposition"; return; }
  init_out="$(run_condukt state init --file "$decomp" 2>&1)"
  init_rc=$?
  if [ "$init_rc" -eq 101 ] || printf '%s' "$init_out" | grep -q 'panicked'; then
    echo "PANIC state init exit=$init_rc: $(printf '%s' "$init_out" | tr '\n' ' ')"
    return
  fi
  rid="$(printf '%s\n' "$init_out" | grep -E '^run-' | tail -1 | tr -d '[:space:]')"
  if [ -z "$rid" ]; then
    echo "REJECTED state init exit=$init_rc produced no run id: $(printf '%s' "$init_out" | tr '\n' ' ')"
    return
  fi
  cc_out="$(run_condukt state check-criteria --run "$rid" --task t1 2>"$errf")"
  cc_rc=$?
  if [ "$cc_rc" -eq 101 ] || grep -q 'panicked' "$errf"; then
    echo "PANIC check-criteria exit=$cc_rc: $(tr '\n' ' ' < "$errf")"
    return
  fi
  printf '%s' "$cc_out" | CC_RC="$cc_rc" python3 -c '
import json, os, sys
rc = os.environ.get("CC_RC")
raw = sys.stdin.read()
try:
    d = json.loads(raw)
except Exception as e:
    print("UNDETERMINED exit=%s unparseable output: %s" % (rc, e)); sys.exit(0)
if not isinstance(d, dict):
    print("UNDETERMINED exit=%s output is not a JSON object" % rc); sys.exit(0)
passed = d.get("passed")
skip = d.get("skip_verifier")
brief = raw.strip().replace("\n", " ")[:300]
# skip_verifier must be an explicit bool; a missing "passed" (no mechanical
# command extracted) is a refusal to pass, which is not a violation as long
# as the verifier is not skipped.
if not isinstance(skip, bool) or (passed is not None and not isinstance(passed, bool)):
    print("UNDETERMINED exit=%s passed=%r skip_verifier=%r (missing or non-bool) json=%s" % (rc, passed, skip, brief)); sys.exit(0)
tag = "VIOLATION" if (passed is True and skip) else "OK"
print("%s exit=%s passed=%r skip_verifier=%r json=%s" % (tag, rc, passed, skip, brief))
'
}

PASS=0
FAIL=0

check_not_skipped() {
  local label="$1" criteria="$2" v
  v="$(classify "$label" "$criteria")"
  case "$v" in
    OK*)
      echo "PASS [$label] not a mechanical pass with verifier skipped: $v"
      PASS=$((PASS + 1)) ;;
    VIOLATION*)
      echo "FAIL [$label] passed+skip_verifier although a named target fails or was not run: $v (criteria: $criteria)"
      FAIL=$((FAIL + 1)) ;;
    *)
      echo "FAIL [$label] cannot determine / panic: $v (criteria: $criteria)"
      FAIL=$((FAIL + 1)) ;;
  esac
}

check_skip_eligible() {
  local label="$1" criteria="$2" v
  v="$(classify "$label" "$criteria")"
  case "$v" in
    VIOLATION*)
      echo "PASS [$label] single green target is still skip-eligible: $v"
      PASS=$((PASS + 1)) ;;
    *)
      echo "FAIL [$label] single green target is not skip-eligible: $v (criteria: $criteria)"
      FAIL=$((FAIL + 1)) ;;
  esac
}

# Degenerate: no panic, and not passed+skip_verifier. A clean rejection at
# state init (REJECTED) is acceptable: it is not a pass.
check_degenerate() {
  local label="$1" criteria="$2" v
  v="$(classify "$label" "$criteria")"
  case "$v" in
    OK*|REJECTED*)
      echo "PASS [$label] no panic and no skipped-verifier pass: $v"
      PASS=$((PASS + 1)) ;;
    VIOLATION*)
      echo "FAIL [$label] degenerate criteria reported passed+skip_verifier: $v (criteria: $criteria)"
      FAIL=$((FAIL + 1)) ;;
    *)
      echo "FAIL [$label] panic / cannot determine: $v (criteria: $criteria)"
      FAIL=$((FAIL + 1)) ;;
  esac
}

check_not_skipped 1 'pytest scripts/test_a.py plus scripts/test_b.py pass'
check_not_skipped 2 'pytest scripts/test_a.py as well as scripts/test_b.py pass'
check_not_skipped 3 'pytest scripts/test_a.py, scripts/test_b.py pass'
check_not_skipped 4 'pytest -x scripts/test_a.py then scripts/test_b.py pass'
check_not_skipped 5 'pytest scripts/test_a.py や scripts/test_b.py が通る'
check_not_skipped 6 'pytest scripts/test_a.py が通る。scripts/test_b.py も通る'
check_not_skipped 7 'python3 -m pytest scripts/test_a.py -k test_ok and scripts/test_b.py pass'
# Case 8: test_bad lives in scripts/test_k.py (separate file so test_a.py
# stays green for the other cases). 8a is the literal "and" expression
# (matches no test); 8b uses "or" so test_bad really runs and fails.
check_not_skipped 8a 'pytest scripts/test_k.py -k "test_ok and test_bad"'
check_not_skipped 8b 'pytest scripts/test_k.py -k "test_ok or test_bad"'
check_skip_eligible 9a 'pytest scripts/test_a.py passes'
check_skip_eligible 9b 'python3 -m pytest scripts/test_a.py passes'
check_degenerate 10a 'pytest'
check_degenerate 10b '   '
check_degenerate 10c 'passes'

echo "$PASS passed, $FAIL failed"
[ "$FAIL" -eq 0 ]
