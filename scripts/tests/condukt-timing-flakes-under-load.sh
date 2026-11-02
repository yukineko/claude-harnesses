#!/usr/bin/env bash
# RED repro: two condukt unit tests are wall-clock-bound and fail under CPU load
# for scheduling reasons, not logic bugs:
#   - worktree::tests::git_fast_subprocess_returns_output_without_timing_out
#     (fake git that sleeps 0 must finish inside a 3s run_git_bounded_with timeout)
#   - lock::backlog_e9262d91::lock_wait_probes_a_changed_holder_pid_immediately
#     (two fake-`kill` probes must both land inside a 120ms deadline; the test
#     itself prints "INCONCLUSIVE (not a pass)" when a probe alone overran it)
#
# Builds the condukt bin test binary once (before any load), then starts LOADERS
# busy-loop processes and runs each test ROUNDS times alone (--test-threads=1)
# with --exact. Exits 1 if ANY run fails (= timing flake reproduced), 0 if all
# pass, 2 if the binary could not be built/located (undetermined, not a pass).
# Measured on this macOS box (8 cpu): 1200 loaders -> worktree test 5/6 fail;
# 400 loaders -> lock test ~3/10 fail; the worktree test needs the higher load.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
LOADERS="${LOADERS:-1200}"
ROUNDS="${ROUNDS:-6}"

bin=$(cargo test -p condukt --bin condukt --no-run --message-format=json 2>/dev/null \
  | python3 -c '
import json, sys
for line in sys.stdin:
    try: m = json.loads(line)
    except Exception: continue
    if m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test") \
       and m.get("target", {}).get("name") == "condukt" and m.get("executable"):
        print(m["executable"])
')
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  echo "UNDETERMINED: could not build/locate the condukt bin test binary"
  exit 2
fi

pids=()
cleanup() { [ ${#pids[@]} -gt 0 ] && kill "${pids[@]}" 2>/dev/null; return 0; }
trap cleanup EXIT
for _ in $(seq "$LOADERS"); do
  ( while :; do :; done ) &
  pids+=($!)
done

fail=0
for t in \
  worktree::tests::git_fast_subprocess_returns_output_without_timing_out \
  lock::backlog_e9262d91::lock_wait_probes_a_changed_holder_pid_immediately; do
  for r in $(seq "$ROUNDS"); do
    if ! out=$("$bin" --exact "$t" --test-threads=1 2>&1); then
      fail=$((fail + 1))
      echo "FAIL [$t] round $r:"
      echo "$out" | grep -A2 panicked
    fi
  done
done

if [ "$fail" -gt 0 ]; then
  echo "RED: $fail timing-bound run(s) failed under $LOADERS busy loops (load, not logic)"
  exit 1
fi
echo "GREEN: all runs passed under $LOADERS busy loops"
echo "test result: ok. $((2 * ROUNDS)) passed; 0 failed"
exit 0
