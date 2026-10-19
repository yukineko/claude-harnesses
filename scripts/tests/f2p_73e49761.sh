#!/usr/bin/env bash
# F2P evidence for backlog 73e49761: continuous_audit_script_behaves must NOT
# pass when bash cannot be spawned. Builds the continuous_audit_script test
# binary FROM THIS CHECKOUT (so at a pre-fix revision the pre-fix test file is
# what runs; backlog's RED overlay brings only this script), runs it with an
# empty PATH, and fails if it reports a pass.
set -u
cd "$(git rev-parse --show-toplevel)" || exit 1
[ -f "$HOME/.cargo/env" ] && . "$HOME/.cargo/env"
json=$(cargo test -p overwatch --test continuous_audit_script --no-run --message-format=json 2>/dev/null) \
  || { echo "UNDETERMINED: could not build continuous_audit_script"; exit 1; }
bin=$(printf '%s\n' "$json" | grep -o '"executable":"[^"]*continuous_audit_script-[^"]*"' | tail -1 | sed 's/^"executable":"//; s/"$//')
[ -x "$bin" ] || { echo "UNDETERMINED: test binary not found in cargo output"; exit 1; }
empty=$(mktemp -d)
out=$(cd "$empty" && env -i PATH= "$bin" continuous_audit_script_behaves --exact 2>&1)
rc=$?
rmdir "$empty"
if [ "$rc" -eq 0 ]; then
  echo "FAIL: continuous_audit_script_behaves PASSED with no bash available"
  printf '%s\n' "$out" | tail -3
  exit 1
fi
echo "ok: the test refuses to pass without bash (exit $rc)"
echo "1 passed"
