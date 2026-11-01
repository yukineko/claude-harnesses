#!/usr/bin/env bash
# Repro: several crates/blastguard/tests/*.rs integration tests build their
# fixtures at a FIXED path under CARGO_TARGET_TMPDIR (a compile-time path,
# identical for every process built from one worktree), then remove_dir_all it
# and recreate it. Two concurrent runs of the same test binary (donegate's Stop
# check + a verifier in one worktree) clobber each other's fixture.
#
# Usage: blastguard-fixed-fixture-race.sh [test-binary-name ...]
# Defaults to the 9 candidate binaries. N (concurrency, default 16) and ROUNDS
# (default 5) come from the environment.
# Per-binary summary: `FAIL <bin>: F/T` or `ok <bin>: T`.
# Exit 1 if ANY binary failed, 2 if a binary could not be built/located
# (undetermined, not a pass), else prints `test result: ok. T passed; 0 failed`.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
N="${N:-16}"
ROUNDS="${ROUNDS:-5}"

if [ "$#" -gt 0 ]; then
  bins=("$@")
else
  bins=(worktree_root_symlink_dotdot backlog_audit_s02 worktree_root_payload_cwd_dotdot \
    heredoc_data_body_extraction backlog_2287ebae approval_memory backlog_6798f47d \
    heredoc_data_body_verify heredoc_data_body_verify2)
fi

targs=()
for b in "${bins[@]}"; do targs+=(--test "$b"); done

map=$(cargo test -p blastguard "${targs[@]}" --no-run --message-format=json 2>/dev/null \
  | python3 -c '
import json, sys
for line in sys.stdin:
    try: m = json.loads(line)
    except Exception: continue
    if m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test") \
       and m.get("executable"):
        print(m["target"]["name"] + " " + m["executable"])
')

logs="$PWD/target/blastguard-fixed-race-logs"
mkdir -p "$logs" || exit 2

undetermined=0
anyfail=0
for b in "${bins[@]}"; do
  bin=$(printf '%s\n' "$map" | awk -v n="$b" '$1 == n { print $2 }' | head -1)
  if [ -z "$bin" ] || [ ! -x "$bin" ]; then
    echo "UNDETERMINED $b: could not build/locate the test binary"
    undetermined=1
    continue
  fi
  fails=0
  for r in $(seq 1 "$ROUNDS"); do
    pids=()
    for i in $(seq 1 "$N"); do
      "$bin" --test-threads=4 >"$logs/$b-$r-$i.log" 2>&1 &
      pids+=("$!")
    done
    for idx in "${!pids[@]}"; do
      if ! wait "${pids[$idx]}"; then
        fails=$((fails + 1))
        f="$logs/$b-$r-$((idx + 1)).log"
        if [ "$fails" -le 2 ]; then
          grep -E "panicked at|Os \{|^test .* FAILED" "$f" | head -3 | sed "s/^/  [$b] /"
        fi
      fi
    done
  done
  total=$((N * ROUNDS))
  if [ "$fails" -ne 0 ]; then
    echo "FAIL $b: $fails/$total"
    anyfail=1
  else
    echo "ok $b: $total"
  fi
done

if [ "$anyfail" -ne 0 ]; then
  echo "FAIL: at least one binary failed under concurrent runs (shared fixture path clobber)"
  exit 1
fi
if [ "$undetermined" -ne 0 ]; then
  exit 2
fi
echo "test result: ok. ${#bins[@]} binaries passed; 0 failed"
