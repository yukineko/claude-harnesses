#!/usr/bin/env bash
# Repro: crates/blastguard/tests/redirect_target_suffix_resolution.rs builds
# `SymlinkFx::new(name)` at a FIXED path,
# `CARGO_TARGET_TMPDIR/redirect_target_suffix_symlink/<name>` (a compile-time
# path, identical for every process built from one worktree), then
# `remove_dir_all`s it and `symlink("/", .../esc").unwrap()`. Two concurrent
# runs of that test binary (donegate's Stop check + a verifier in one worktree)
# race: one's create_dir_all/symlink lands between the other's remove and
# symlink, and the symlink unwrap panics with AlreadyExists (os error 17).
#
# Builds the integration test binary once, then runs it N times concurrently
# against --exact the three SymlinkFx tests. Prints
# `test result: ok. N passed; 0 failed` only when every run passes.
# Exits 1 if ANY run fails (defect reproduced), 2 if the binary could not be
# built/located (undetermined, not a pass).
set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
N="${N:-16}"
ROUNDS="${ROUNDS:-5}"

bin=$(cargo test -p blastguard --test redirect_target_suffix_resolution --no-run --message-format=json 2>/dev/null \
  | python3 -c '
import json, sys
for line in sys.stdin:
    try: m = json.loads(line)
    except Exception: continue
    if m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test") \
       and m.get("target", {}).get("name") == "redirect_target_suffix_resolution" and m.get("executable"):
        print(m["executable"])
')
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  echo "UNDETERMINED: could not build/locate the redirect_target_suffix_resolution test binary"
  exit 2
fi

logs="$PWD/target/blastguard-symlink-race-logs"
mkdir -p "$logs" || exit 2

fails=0
for r in $(seq 1 "$ROUNDS"); do
  pids=()
  for i in $(seq 1 "$N"); do
    "$bin" --exact \
      symlink_before_dotdot_in_a_suffix_must_not_be_resolved \
      symlink_without_a_dotdot_is_caught_by_the_symlink_axis \
      symlink_fixture_still_allows_an_ordinary_confined_suffix \
      --test-threads=3 >"$logs/run-$r-$i.log" 2>&1 &
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
  echo "FAIL: $fails/$total concurrent runs failed (shared fixture path clobber)"
  exit 1
fi
echo "PASS: $total/$total concurrent runs passed"
echo "test result: ok. $total passed; 0 failed"
