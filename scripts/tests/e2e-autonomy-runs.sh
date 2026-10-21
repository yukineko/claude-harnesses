#!/usr/bin/env bash
# Regression test: scripts/e2e-autonomy.sh must actually RUN its demo and pass.
#
# Observed 2026-10-22: step 2 of e2e-autonomy.sh (`backlog add --project
# /tmp/e2e-autonomy-demo-project` under a temp HOME) is refused by backlog's
# one-store-per-repo rule:
#   * from a linked worktree: "this store belongs to project <repo> ... cannot
#     hold tasks for --project /tmp/e2e-autonomy-demo-project";
#   * from the primary working tree: backlog add is refused there;
#   * from outside any repo: "no git repo above <cwd>".
# So the script exits 1 ("backlog add failed") wherever it is run.
#
# This test forces the autonomy switch ON (CONDUKT_AUTONOMOUS=1) so the script
# cannot leave through its "autonomy disabled -> SKIP, exit 0" door: a SKIP
# would make a pass here vacuous, so it is a FAIL. It also asserts the demo did
# not write into THIS repo's backlog store.
set -uo pipefail

REPO="$(cd "$(dirname "$0")/../.." && pwd)"
SCRIPT="$REPO/scripts/e2e-autonomy.sh"

fail() { echo "FAIL: $*" >&2; exit 1; }
pass() { echo "  ok: $*"; }

[ -f "$SCRIPT" ] || fail "$SCRIPT missing"

store="$REPO/.backlog/tasks.toml"
before=""
[ -f "$store" ] && before="$(shasum -a 256 "$store")"

out="$(cd "$REPO" && CONDUKT_AUTONOMOUS=1 bash "$SCRIPT" 2>&1)"
rc=$?

after=""
[ -f "$store" ] && after="$(shasum -a 256 "$store")"

if [ "$rc" -ne 0 ]; then
  printf '%s\n' "$out" | tail -15 >&2
  fail "e2e-autonomy.sh exited $rc"
fi
pass "e2e-autonomy.sh exited 0"

if printf '%s\n' "$out" | grep -q '^RESULT: SKIP'; then
  printf '%s\n' "$out" | tail -5 >&2
  fail "e2e-autonomy.sh SKIPPED even with CONDUKT_AUTONOMOUS=1 — nothing was demonstrated"
fi
printf '%s\n' "$out" | grep -q '^RESULT: PASS (4/4' \
  || { printf '%s\n' "$out" | tail -5 >&2; fail "no 'RESULT: PASS (4/4' line"; }
pass "RESULT: PASS (4/4)"

[ "$before" = "$after" ] || fail "the demo modified this repo's backlog store $store"
pass "this repo's backlog store is untouched"

echo "PASS: e2e-autonomy.sh runs its demo end to end (3 passed)"
