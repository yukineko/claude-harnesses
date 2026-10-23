#!/usr/bin/env bash
# F2P evidence for backlog fc9e3487: rebuild-stage-repo-destination.sh must pass
# on this host (it hard-coded backlog-linux-x86_64, so on any other host the
# refresh loop scanned nothing and the test failed its own vacuity check). The
# fix lives in that test script, so this wrapper runs it FROM THIS CHECKOUT
# (backlog's RED overlay brings only this wrapper, so at the pre-fix revision
# the pre-fix test script is what runs).
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
out=$(bash scripts/tests/rebuild-stage-repo-destination.sh 2>&1)
rc=$?
printf '%s\n' "$out" | grep -E '^(FAIL|PASS|  ok)' | head -5
if [ "$rc" -ne 0 ]; then
  echo "FAIL: rebuild-stage-repo-destination.sh exited $rc"
  exit 1
fi
echo "1 passed"
