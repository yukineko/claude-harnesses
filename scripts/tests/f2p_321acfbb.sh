#!/usr/bin/env bash
# F2P evidence for backlog 321acfbb: the CA-script repro must pick the newest
# sibling build, not the last by file name. Runs the picker test FROM THIS
# CHECKOUT (backlog's RED overlay brings only this script, so at the pre-fix
# revision the pre-fix picker is what runs).
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
out=$(cargo test -p overwatch --test repro_ca_script_test_passes_without_bash pick_sibling -- --include-ignored 2>&1)
rc=$?
printf '%s\n' "$out" | grep -E 'test result|panicked|picked a stale' | head -5
if [ "$rc" -ne 0 ]; then
  echo "FAIL: pick_sibling chose a stale build (cargo exit $rc)"
  exit 1
fi
echo "1 passed"
