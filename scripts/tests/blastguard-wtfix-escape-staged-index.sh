#!/usr/bin/env bash
# Repro (escape): concurrent runs of blastguard's worktree_protected_paths_adversarial
# test, cwd = a checkout whose index has STAGED content, with the DEFAULT fixture
# location (CARGO_TARGET_TMPDIR under that checkout), make a fixture
# `git commit -q -m init` (author t <t@example.invalid>) land in the enclosing
# checkout: it commits the staged index (no `git add` needed).
#
# Works only on a throwaway clone under target/ of the current checkout; never
# touches the real checkout. Exit 1 = escape reproduced (clone HEAD gained a
# t@example.invalid commit), 0 = not observed, 2 = undetermined (build failed).
set -u
root=$(git rev-parse --show-toplevel) || exit 2
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
N="${N:-6}"
ROUNDS="${ROUNDS:-12}"
clone="$root/target/wtfix-escape-clone"
rm -r "$clone" 2>/dev/null
git clone -q "$root" "$clone" || exit 2
cd "$clone" || exit 2
echo staged >STAGED.txt && git add STAGED.txt || exit 2
unset BLASTGUARD_TEST_FIXTURE_DIR

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
  echo "UNDETERMINED: could not build/locate the test binary in the clone"
  exit 2
fi

for r in $(seq 1 "$ROUNDS"); do
  for i in $(seq 1 "$N"); do
    "$bin" --test-threads=4 >/dev/null 2>&1 &
  done
  wait
done

escaped=$(git log --format='%h %an <%ae> %s' --author='t@example.invalid')
if [ -n "$escaped" ]; then
  echo "FAIL: fixture commit escaped into the enclosing clone:"
  echo "$escaped"
  git show --stat --format='%h' HEAD | tail -3
  exit 1
fi
total=$((N * ROUNDS))
echo "no fixture commit escaped in $total concurrent runs"
echo "test result: ok. $total passed; 0 failed"
