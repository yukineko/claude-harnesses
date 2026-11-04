#!/usr/bin/env bash
# Repro: condukt's capstone e2e tests leave their fixture dirs in the temp dir.
#
# foundation_capstone_e2e.rs and schedule_conflict_capstone_e2e.rs build a
# Fixture under std::env::temp_dir() (condukt-capstone-e2e-<pid>-<tag>,
# condukt-schedule-capstone-e2e-<pid>-<tag>) and only remove a stale one at
# the START of a run; nothing removes it at the end. Observed 2026-11-02: 190
# condukt-capstone-e2e-*-capstone dirs (119M) in /tmp dating 07-10..10-06.
#
# The tests run with TMPDIR pointed at a fresh empty dir; after they finish
# (pass or fail) that dir must hold nothing they created.
# Exit 0 = no leak (prints a `test result: ok. 1 passed` line for closure
# evidence); exit 1 = leak (RED); exit 2 = could not determine.
set -u
repo="$(cd "$(dirname "$0")/../.." && pwd)" || exit 2
# shellcheck disable=SC1091
. "$HOME/.cargo/env" 2>/dev/null || true
probe="$(mktemp -d)" || exit 2
trap 'rm -rf "$probe"' EXIT

rc=0
TMPDIR="$probe" cargo test --manifest-path "$repo/Cargo.toml" -p condukt \
  --test foundation_capstone_e2e --test schedule_conflict_capstone_e2e \
  >"$probe.log" 2>&1 || rc=$?
if ! grep -q '^test result:' "$probe.log"; then
  echo "UNDETERMINED: the capstone tests did not run (cargo rc=$rc):" >&2
  tail -20 "$probe.log" >&2
  rm -f "$probe.log"
  exit 2
fi
rm -f "$probe.log"

leaked="$(find "$probe" -mindepth 1 -maxdepth 1 -name 'condukt-*capstone-e2e-*' -print)"
if [ -n "$leaked" ]; then
  echo "LEAK: capstone e2e fixtures left in TMPDIR after the run (cargo rc=$rc):"
  echo "$leaked" | sed "s|^$probe/|  |"
  exit 1
fi
echo "OK: no capstone e2e fixture left in TMPDIR (cargo rc=$rc)"
echo "test result: ok. 1 passed; 0 failed"
exit 0
