#!/usr/bin/env bash
# Behavioural test for RS_UNDET_ARM_EMPTY (`undetermined-arm-empty-fallback`)
# in scripts/check-fail-open.py. Each case builds a throwaway git repo holding a
# copy of the scanner (REPO is derived from the script's own location) plus one
# Rust file with one arm, runs `check-fail-open.py --all` (advisory: documented
# exit 0) and asserts on the pattern name in the output.
set -u

HERE="$(cd "$(dirname "$0")" && pwd)" || { echo "cannot resolve script dir" >&2; exit 2; }
SRC="$(cd "$HERE/.." && pwd)/check-fail-open.py"
[ -f "$SRC" ] || { echo "missing scanner: $SRC" >&2; exit 2; }

PAT='undetermined-arm-empty-fallback'
pass=0
fail=0
tmps=()
cleanup() { for d in "${tmps[@]:-}"; do [ -n "$d" ] && rm -rf "$d"; done; }
trap cleanup EXIT

# run_case <name> <expect: flagged|clean> <rust arm line>
run_case() {
  local name="$1" expect="$2" arm="$3" d out rc
  d="$(mktemp -d)" || { echo "FAIL $name: mktemp failed"; fail=$((fail+1)); return; }
  tmps+=("$d")
  mkdir -p "$d/scripts" "$d/crates/blastguard/src" || { echo "FAIL $name: mkdir"; fail=$((fail+1)); return; }
  cp "$SRC" "$d/scripts/check-fail-open.py" || { echo "FAIL $name: cp"; fail=$((fail+1)); return; }
  printf 'fn f(x: R) -> T {\n    match x {\n        %s,\n        _ => unreachable!(),\n    }\n}\n' "$arm" \
    > "$d/crates/blastguard/src/x.rs" || { echo "FAIL $name: write"; fail=$((fail+1)); return; }
  (
    cd "$d" || exit 90
    git init -q . || exit 91
    git add -A || exit 92
    git -c user.email=t@t -c user.name=t -c commit.gpgsign=false commit -q -m init || exit 93
  ) || { echo "FAIL $name: git setup rc=$?"; fail=$((fail+1)); return; }
  out="$(cd "$d" && python3 scripts/check-fail-open.py --all 2>&1)"
  rc=$?
  if [ "$rc" -ne 0 ]; then
    echo "FAIL $name: --all is documented advisory (exit 0), got rc=$rc"; fail=$((fail+1)); return
  fi
  local hit=0
  printf '%s\n' "$out" | grep -q "\[$PAT\]" && hit=1
  if [ "$expect" = flagged ] && [ "$hit" -eq 1 ]; then
    echo "ok   $name (flagged)"; pass=$((pass+1))
  elif [ "$expect" = clean ] && [ "$hit" -eq 0 ]; then
    echo "ok   $name (not flagged)"; pass=$((pass+1))
  else
    echo "FAIL $name: expected $expect, pattern-hit=$hit"
    printf '%s\n' "$out" | sed 's/^/     | /'
    fail=$((fail+1))
  fi
}

run_case blocked-vec-new        flagged 'Blocked(_) => Vec::new()'
run_case undet-default          flagged 'Undetermined(_) => Default::default()'
run_case det-undet-string-new   flagged 'Determination::Undetermined(_) => String::new()'
run_case required-blocked-none  flagged 'Required::Blocked(_) => None'
run_case undet-none             flagged 'Undetermined(_) => None'
run_case blocked-forward-return clean   'Blocked(why) => return why'
run_case undet-forward          clean   'Undetermined(why) => Determination::Undetermined(why)'
run_case some-x-none            clean   'Some(x) => None'
# Err(_) => None is out of scope: deliberately not asserted either way.
# Blocked(_) => Some(None) skipped: odd shape, no defined expectation.

echo "$pass passed, $fail failed"
[ "$fail" -eq 0 ]
