#!/usr/bin/env bash
# RED repro: blastguard lib test
#   detect::tests::d4_exponential_find_exec_is_bounded_and_denies
# asserts the analysis of an exponential `find -exec` finishes in < 2s of WALL
# time. It is meant to prove the analysis is BOUNDED (budget exhaustion yields
# Ask/Deny, never a hang or an Allow), but a wall-clock bound is load-sensitive:
# under CPU contention it fails for scheduling reasons (observed 2.29s, 3.5s,
# 6.43s; ~0.6s alone), and so keeps failing donegate Stop checks.
#
# Modelled on scripts/tests/condukt-timing-flakes-under-load.sh (c77cdc98).
# Builds the blastguard lib test binary once (before any load), starts LOADERS
# busy-loop processes, runs the one test ROUNDS times with --exact. Exits 1 if
# ANY run fails, 0 if all pass, 2 if the binary could not be built/located
# (undetermined, not a pass). Busy loops are always killed (trap).
set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
# Measured on this macOS box (8 cpu): 40 loaders -> 3/3 fail at 25-29s each
# (~2 min total); 400 loaders -> 6/6 fail at ~165s each (~18 min, too slow).
LOADERS="${LOADERS:-40}"
ROUNDS="${ROUNDS:-3}"
T=detect::tests::d4_exponential_find_exec_is_bounded_and_denies

bin=$(cargo test -p blastguard --lib --no-run --message-format=json 2>/dev/null \
  | python3 -c '
import json, sys
for line in sys.stdin:
    try: m = json.loads(line)
    except Exception: continue
    if m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test") \
       and m.get("target", {}).get("name") == "blastguard" and m.get("executable"):
        print(m["executable"])
')
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  echo "UNDETERMINED: could not build/locate the blastguard lib test binary"
  exit 2
fi

pids=()
cleanup() { [ ${#pids[@]} -gt 0 ] && kill "${pids[@]}" 2>/dev/null; return 0; }
trap cleanup EXIT INT TERM
for _ in $(seq "$LOADERS"); do
  ( while :; do :; done ) &
  pids+=($!)
done

fail=0
for r in $(seq "$ROUNDS"); do
  if ! out=$("$bin" --exact "$T" --test-threads=1 2>&1); then
    fail=$((fail + 1))
    echo "FAIL [$T] round $r:"
    echo "$out" | grep -A2 panicked
  fi
done

if [ "$fail" -gt 0 ]; then
  echo "RED: $fail/$ROUNDS run(s) failed under $LOADERS busy loops (wall-clock bound, load not logic)"
  exit 1
fi
echo "GREEN: all runs passed under $LOADERS busy loops"
echo "test result: ok. $ROUNDS passed; 0 failed"
exit 0
