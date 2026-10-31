#!/usr/bin/env bash
# Repro: blastguard's worktree_protected_paths_adversarial fixtures build a git
# repo under a SHARED, fixed base (neutral_base()/worktree_protected_paths_adversarial/<name>)
# and `remove_dir_all` it first. Concurrent runs sharing that base delete each
# other's fixture mid-setup; a later fixture `git` command then discovers the
# ENCLOSING repo (the real checkout the target dir lives in) and `git add -A` /
# `git commit -m init` (author t <t@example.invalid>) land in it.
#
# This script never touches the real checkout: it makes a throwaway sandbox git
# repo (committer "sandbox") under target/, points the test at
# $sandbox/fixtures via BLASTGUARD_TEST_FIXTURE_DIR, runs the prebuilt test
# binary N times concurrently. RED (exit 1) if ANY run fails (the collision) or
# the sandbox repo gains a commit by t@example.invalid (the escape). 0 = neither,
# 2 = could not build/locate the binary (undetermined, not a pass).
set -u
cd "$(git rev-parse --show-toplevel)" || exit 2
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
N="${N:-6}"
ROUNDS="${ROUNDS:-8}"

bin=$(cargo test -p blastguard --test worktree_protected_paths_adversarial --no-run --message-format=json 2>/dev/null \
  | python3 -c '
import json, sys
for line in sys.stdin:
    try: m = json.loads(line)
    except Exception: continue
    if m.get("reason") == "compiler-artifact" and m.get("profile", {}).get("test") \
       and m.get("target", {}).get("name") == "worktree_protected_paths_adversarial" and m.get("executable"):
        print(m["executable"])
')
if [ -z "$bin" ] || [ ! -x "$bin" ]; then
  echo "UNDETERMINED: could not build/locate the test binary"
  exit 2
fi

sandbox="$PWD/target/wtfix-escape-sandbox"
rm -r "$sandbox" 2>/dev/null
mkdir -p "$sandbox/fixtures" || exit 2
sg() { git -C "$sandbox" -c user.name=sandbox -c user.email=sandbox@example.invalid -c core.hooksPath=/dev/null "$@"; }
sg init -q || exit 2
sg commit -q --allow-empty -m sandbox-root || exit 2
logs="$sandbox/../wtfix-escape-logs"
mkdir -p "$logs" || exit 2

fails=0
for r in $(seq 1 "$ROUNDS"); do
  pids=()
  for i in $(seq 1 "$N"); do
    BLASTGUARD_TEST_FIXTURE_DIR="$sandbox/fixtures" "$bin" --test-threads=4 >"$logs/run-$r-$i.log" 2>&1 &
    pids+=("$!")
  done
  for p in "${pids[@]}"; do wait "$p" || fails=$((fails + 1)); done
done

echo "sample errors seen:"
grep -h "unable to get current working directory\|failed in" "$logs"/run-*.log 2>/dev/null | sort | uniq -c | head -5

escaped=$(sg log --format='%h %an <%ae> %s' --author='t@example.invalid' 2>/dev/null)
if [ -n "$escaped" ]; then
  echo "FAIL: fixture commit(s) by t@example.invalid escaped into the enclosing repo:"
  echo "$escaped"
  exit 1
fi
total=$((N * ROUNDS))
if [ "$fails" -ne 0 ]; then
  echo "FAIL: $fails/$total concurrent runs failed (shared fixture dir clobbered)"
  exit 1
fi
echo "no fixture commit escaped in $total concurrent runs"
echo "test result: ok. $total passed; 0 failed"
