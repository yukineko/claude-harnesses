#!/bin/bash
# Whole-block test: the scout SKILL.md Phase-5 bash block must fail closed.
#
# Unlike scout-skill-discovery-exit-status.sh (which runs each compass line in
# isolation and so cannot see the `|| { ...; exit 1; }` handlers or the jq
# OWNED shape check), this extracts the Phase-5 bash fence VERBATIM from
# SKILL.md and runs the WHOLE block with stub `compass` / `backlog` first on PATH.
#
# Block selection: the bash fence that contains `compass discovery list`
# AND `compass discovery record`.
#
# Substitutions (the only edits to the extracted text, all mechanical):
#   1. every `<...>` span with no `<`, `>` or `"` inside -> `PLACEHOLDER`
#      (the skill's fill-in text such as <p0|p1|p2>, <lens>, "<rationale> ...";
#      unquoted ones would otherwise be shell redirections/pipes).
#   2. the loop variable `施策名` -> `TASKNAME` (non-ASCII identifier; bash in a
#      non-UTF-8 locale rejects it). Done with sed on the whole text, so
#      `$施策名` / `"$施策名"` inside strings become `$TASKNAME` too.
# Prelude (not in the block, supplied by the model at runtime): SESSION_ID=me,
# APPROVED_TASKS=(task-one) (case A0 uses an empty list).
#
# Cases: A list fails; B list=[] but record fails; C list invalid JSON / empty
# with exit 0; D control all-success (anti-vacuity: backlog add IS invoked).
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SKILL="$ROOT/crates/scout/skills/scout/SKILL.md"
[ -f "$SKILL" ] || { echo "ABORT: $SKILL not found" >&2; exit 2; }

fails=0
pass() { echo "  ok   - $1"; }
fail() { echo "  FAIL - $1" >&2; fails=$((fails + 1)); }

tmproot="${TMPDIR:-/tmp}"; tmproot="${tmproot%/}"
work="$(mktemp -d "$tmproot/scout-phase5.XXXXXX")" || { echo "ABORT: mktemp" >&2; exit 2; }
[ -n "$work" ] && [ -d "$work" ] || { echo "ABORT: mktemp gave no dir" >&2; exit 2; }
trap 'rm -r -f -- "$work"' EXIT

# Extract bash fences, one file per fence; pick the Phase-5 one.
awk -v d="$work" '
  /^[[:space:]]*[`][`][`]bash[[:space:]]*$/ { inb = 1; n++; next }
  /^[[:space:]]*[`][`][`][[:space:]]*$/ { inb = 0; next }
  inb { print > (d "/fence." n) }
' "$SKILL"
block_raw=""
for f in "$work"/fence.*; do
  [ -f "$f" ] || continue
  if grep -q 'compass discovery list' "$f" && grep -q 'compass discovery record' "$f"; then
    block_raw="$f"; break
  fi
done
[ -n "$block_raw" ] || { echo "FAIL - no bash fence with list+record found" >&2; exit 1; }

sed -e 's/<[^<>"]*>/PLACEHOLDER/g' -e 's/施策名/TASKNAME/g' "$block_raw" > "$work/block.body"
{ echo 'SESSION_ID=me'; echo 'APPROVED_TASKS=(${TEST_TASKS-task-one})'; cat "$work/block.body"; } > "$work/block.sh"

[ -s "$work/block.body" ] && pass "extracted block non-empty" || fail "extracted block empty"
grep -q 'compass discovery list' "$work/block.body" && pass "block has list invocation" || fail "no list invocation"
grep -q 'compass discovery record' "$work/block.body" && pass "block has record invocation" || fail "no record invocation"

mkdir -p "$work/bin"
cat > "$work/bin/compass" <<'STUB'
#!/bin/sh
echo "compass $*" >> "$STUB_LOG"
case "$*" in
  *"discovery list"*)
    case "$LIST_MODE" in
      fail) echo "compass: discovery store undetermined (test stub)" >&2; exit 3;;
      garbage) echo "garbage"; exit 0;;
      empty) exit 0;;
      *) echo "[]"; exit 0;;
    esac;;
  *"discovery record"*)
    if [ "$RECORD_MODE" = fail ]; then echo "compass: record undetermined (test stub)" >&2; exit 3; fi
    exit 0;;
esac
exit 0
STUB
cat > "$work/bin/backlog" <<'STUB'
#!/bin/sh
echo "backlog $*" >> "$STUB_LOG"
exit 0
STUB
chmod +x "$work/bin/compass" "$work/bin/backlog"

# run_case <list_mode> <record_mode>; sets rc, errout, log
run_case() {
  export TEST_TASKS="${TEST_TASKS-task-one}" LIST_MODE="$1" RECORD_MODE="$2" STUB_LOG="$work/log"
  : > "$STUB_LOG"
  PATH="$work/bin:$PATH" bash "$work/block.sh" >"$work/out" 2>"$work/err"
  rc=$?
  errout="$(cat "$work/err")"
  log="$(cat "$STUB_LOG")"
}
backlog_called() { printf '%s\n' "$log" | grep -q '^backlog add'; }
record_called() { printf '%s\n' "$log" | grep -q 'discovery record'; }

echo "A0: list fails, zero approved tasks (loop body never runs)"
TEST_TASKS="" run_case fail ok
[ "$rc" -ne 0 ] && pass "A0 exits non-zero (rc=$rc)" || fail "A0 exited 0 (list failure swallowed)"

echo "A: list fails"
TEST_TASKS="task-one" run_case fail ok
[ "$rc" -ne 0 ] && pass "A exits non-zero (rc=$rc)" || fail "A exited 0"
printf '%s' "$errout" | grep -q 'undetermined\|判定不能\|中止' && pass "A stderr surfaces failure" || fail "A stderr silent: $errout"
backlog_called && fail "A backlog add invoked" || pass "A no backlog add"
record_called && fail "A record invoked" || pass "A no record"

echo "B: list [] but record fails"
run_case ok fail
[ "$rc" -ne 0 ] && pass "B exits non-zero (rc=$rc)" || fail "B exited 0"
record_called && pass "B record was attempted (non-vacuous)" || fail "B record never attempted"
backlog_called && fail "B backlog add invoked" || pass "B no backlog add"

echo "C: list garbage / empty with exit 0"
for m in garbage empty; do
  run_case "$m" ok
  [ "$rc" -ne 0 ] && pass "C($m) exits non-zero (rc=$rc)" || fail "C($m) exited 0"
  backlog_called && fail "C($m) backlog add invoked" || pass "C($m) no backlog add"
done

echo "D: control all succeed"
run_case ok ok
[ "$rc" -eq 0 ] && pass "D exits 0" || fail "D rc=$rc: $errout"
backlog_called && pass "D backlog add invoked" || fail "D backlog add NOT invoked"

[ "$fails" -eq 0 ] && { echo "ALL PASS"; exit 0; }
echo "$fails failure(s)" >&2; exit 1
