#!/bin/bash
# The scout skill must not erase a failing `compass discovery list` / `record`.
#
# crates/scout/skills/scout/SKILL.md tells the model to run, in Phase 5:
#   DISCOVERED=$(compass discovery list --json 2>/dev/null || echo '[]')
#   compass discovery record ... 2>/dev/null || true
# The first turns an undetermined discovery store (compass exits non-zero and
# says `undetermined` on stderr) into `[]` = "nobody discovered anything", so
# every duplicate is re-filed; the second turns a record that was NOT stored
# into success. Both also discard compass's stderr, so the reason is invisible.
#
# Behavioural check: every line inside a ```bash fence of SKILL.md that invokes
# `compass discovery` is executed, verbatim, with a stub `compass` first on PATH
# that prints `compass: discovery store undetermined (test stub)` to stderr and
# exits 3. Each line runs inside a one-iteration `for` loop (so `continue` /
# `break` are legal) in its own bash process. A line passes only if ALL hold:
#   (1) the run's exit status is non-zero (the failure is not turned into success);
#   (2) stderr contains `undetermined` (the failure is surfaced, not swallowed);
#   (3) if the line assigns DISCOVERED, DISCOVERED is not the empty JSON array.
#
# Control (anti-vacuity): the same harness with a stub that succeeds (prints
# `[]` / nothing, exits 0) must make every line exit 0 — otherwise (1) would be
# satisfied by a broken harness rather than by the line propagating failure.
# Also asserts at least one `compass discovery list` and one
# `compass discovery record` line exist, so deleting them cannot pass vacuously.
#
# What this does NOT prove: it executes each invocation line in isolation, not
# the whole Phase-5 block (which contains `<placeholder>` text and is not
# runnable as-is), so it does not prove the surrounding loop/jq logic reacts
# correctly to a surfaced failure, nor that the model actually follows the
# skill. A multi-line construct around an invocation (e.g. `if ! X=$(...); then`
# split over lines) is rejected as a syntax error of the isolated line; keep
# each invocation and its failure handling on one line.
set -uo pipefail

ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
SKILL="$ROOT/crates/scout/skills/scout/SKILL.md"
[ -f "$SKILL" ] || { echo "ABORT: $SKILL not found" >&2; exit 2; }

fails=0
pass() { echo "  ok   - $1"; }
fail() { echo "  FAIL - $1" >&2; fails=$((fails + 1)); }

tmproot="${TMPDIR:-/tmp}"; tmproot="${tmproot%/}"
work="$(mktemp -d "$tmproot/scout-discovery.XXXXXX")" || { echo "ABORT: mktemp" >&2; exit 2; }
[ -n "$work" ] && [ -d "$work" ] || { echo "ABORT: mktemp gave no dir" >&2; exit 2; }
trap 'rm -r -f -- "$work"' EXIT

mkdir -p "$work/fail-bin" "$work/ok-bin"
printf '%s\n' '#!/bin/sh' \
  'echo "compass: discovery store undetermined (test stub)" >&2' \
  'exit 3' > "$work/fail-bin/compass"
printf '%s\n' '#!/bin/sh' \
  'case "$*" in *list*) echo "[]";; esac' \
  'exit 0' > "$work/ok-bin/compass"
chmod +x "$work/fail-bin/compass" "$work/ok-bin/compass"

# Lines inside ```bash fences that invoke `compass discovery`.
awk '
  /^[[:space:]]*```bash[[:space:]]*$/ { inb = 1; next }
  /^[[:space:]]*```/ { inb = 0; next }
  inb && /compass discovery/ && $0 !~ /^[[:space:]]*#/ { print }
' "$SKILL" > "$work/lines"

n_list=$(grep -c 'compass discovery list' "$work/lines" || true)
n_record=$(grep -c 'compass discovery record' "$work/lines" || true)
if [ "${n_list:-0}" -ge 1 ] && [ "${n_record:-0}" -ge 1 ]; then
  pass "found $n_list list / $n_record record invocation line(s) in bash fences"
else
  fail "expected >=1 'compass discovery list' and >=1 'record' line in bash fences; got list=$n_list record=$n_record"
fi

# Run one line with the given stub dir; sets RC, ERR, DISC.
run_line() {
  local line="$1" bindir="$2"
  local script="$work/case.sh"
  {
    echo 'SESSION_ID=test-session'
    echo 'trap '"'"'printf "%s" "${DISCOVERED-__UNSET__}" > "$DISC_OUT"'"'"' EXIT'
    echo 'for __scout_once in 1; do'
    printf '%s\n' "$line"
    echo 'done'
  } > "$script"
  if ! bash -n "$script" 2>"$work/syntax"; then
    RC=syntax; ERR="$(cat "$work/syntax")"; DISC=""
    return
  fi
  : > "$work/disc"
  DISC_OUT="$work/disc" PATH="$bindir:$PATH" bash "$script" >/dev/null 2>"$work/err"
  RC=$?
  ERR="$(cat "$work/err")"
  DISC="$(cat "$work/disc")"
}

i=0
while IFS= read -r line; do
  i=$((i + 1))
  label="line $i: $(printf '%s' "$line" | sed 's/^[[:space:]]*//')"

  run_line "$line" "$work/ok-bin"
  if [ "$RC" = "syntax" ]; then
    fail "$label -- not a self-contained shell line: $ERR"
    continue
  fi
  if [ "$RC" -ne 0 ]; then
    fail "$label -- CONTROL: with a succeeding compass stub the line must exit 0, got $RC (stderr: $ERR)"
    continue
  fi

  run_line "$line" "$work/fail-bin"
  ok=1
  if [ "$RC" -eq 0 ]; then
    fail "$label -- compass exited 3 (undetermined) but the line exited 0: failure erased into success"
    ok=0
  fi
  if ! printf '%s' "$ERR" | grep -qi 'undetermined'; then
    fail "$label -- compass said 'undetermined' on stderr but nothing reached stderr: failure not surfaced (stderr: '$ERR')"
    ok=0
  fi
  case "$line" in
    *DISCOVERED=*)
      if printf '%s' "$DISC" | tr -d '[:space:]' | grep -qx '\[\]'; then
        fail "$label -- an undetermined store became DISCOVERED='[]' (read downstream as 'nobody discovered anything')"
        ok=0
      fi
      ;;
  esac
  [ "$ok" -eq 1 ] && pass "$label -- failure propagated (exit $RC) and surfaced"
done < "$work/lines"

if [ "$fails" -ne 0 ]; then
  echo "FAILED: $fails check(s)" >&2
  exit 1
fi
echo "PASSED"
