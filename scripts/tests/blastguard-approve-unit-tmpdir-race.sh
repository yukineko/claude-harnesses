#!/usr/bin/env bash
# Repro: blastguard's approve.rs unit-test helper `store_dir(name)` uses a FIXED
# path, `std::env::temp_dir()/blastguard-approve-unit/<name>`, and
# `remove_dir_all`s it first. Two concurrent `cargo test -p blastguard --lib`
# processes sharing one $TMPDIR (e.g. donegate's Stop check in one worktree and
# an agent's run in another) therefore clobber each other's store.
#
# Builds the lib test binary once, then runs it N times concurrently with
# --exact on the two store tests that were observed to fail, all sharing one
# TMPDIR. Exits 1 if ANY run fails (= defect reproduced), 0 if all pass,
# 2 if the binary could not be built/located (undetermined, not a pass).
set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
N="${N:-16}"
ROUNDS="${ROUNDS:-5}"

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

# One shared TMPDIR for every run (that is the condition under test). Kept
# inside target/ so nothing lands in the system temp dir.
shared_tmp="$PWD/target/blastguard-approve-race-tmp"
logs="$shared_tmp/logs"
mkdir -p "$logs" || exit 2

fails=0
for r in $(seq 1 "$ROUNDS"); do
  pids=()
  for i in $(seq 1 "$N"); do
    TMPDIR="$shared_tmp" "$bin" --exact \
      approve::tests::pending_is_not_an_approval_until_promoted \
      approve::tests::promote_consumes_the_pending_entry \
      --test-threads=2 >"$logs/run-$r-$i.log" 2>&1 &
    pids+=("$!")
  done
  for idx in "${!pids[@]}"; do
    if ! wait "${pids[$idx]}"; then
      fails=$((fails + 1))
      f="$logs/run-$r-$((idx + 1)).log"
      echo "--- FAILED run $r/$((idx + 1)) ---"
      grep -E "panicked at|left:|right:|Os \{|^test .* FAILED" "$f" | head -8
    fi
  done
done

total=$((N * ROUNDS))
if [ "$fails" -ne 0 ]; then
  echo "FAIL: $fails/$total concurrent runs failed (shared TMPDIR clobber)"
  exit 1
fi
echo "PASS: $total/$total concurrent runs passed"
echo "test result: ok. $total passed; 0 failed"
