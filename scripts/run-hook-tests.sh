#!/bin/sh
# The git-hook test suites that are GREEN, run as one donegate check (backlog
# c3a98510).
#
# These three suites — test_precommit_hook.py, test_git_hook_coverage.py and
# test_gate_bypass.py — exercise the hooks in .githooks/, which are the actual
# block/allow authority in this repo (CLAUDE.md 7: the gate's substance stays
# local, never in CI). Nothing executed any of them. `scripts/check-workspace-
# tests.py` would (it globs scripts/test_*.py), but nothing invokes THAT either,
# so the suites guarding the guards were themselves unguarded.
#
# WHY ONLY TWO OF THE THREE ARE HERE. Measured 2026-09-13 at aed66762:
#
#   test_precommit_hook.py    Ran 16 tests in 27.783s   OK
#   test_git_hook_coverage.py Ran  9 tests in  7.227s   OK
#   test_gate_bypass.py       Ran 124 tests in 75.616s  FAILED (failures=1)
#
# The two green ones run HERE, as a required check. test_gate_bypass runs as a
# separate ADVISORY check (see donegate.toml), because its one failure is the
# open defect backlog 6267bfbe pins — the post-commit amend certificate. Wiring
# a suite with a live red into the blocking path would block every stop in the
# repo, and the pressure that creates is precisely what produces a shared skip
# file (CLAUDE.md 5) or a quietly-deleted assertion (CLAUDE.md 4).
#
# It is deliberately NOT silenced: it still runs, still reports, and the moment
# 6267bfbe lands it should be moved into this script and the advisory entry
# deleted. Splitting green-from-red is the honest way to enforce what passes
# today without either lying about the red or being blocked by it.
#
# Latency: ~35s combined, and `when_changed` in donegate.toml keeps it off turns
# that touch neither the hooks nor the hook scripts.
set -eu

repo="$(cd "$(dirname "$0")/.." && pwd)"
cd "$repo"

py="${PYTHON:-python3}"
command -v "$py" >/dev/null 2>&1 || {
    # An absent interpreter is NOT a pass. Exiting 0 here would report the hook
    # suites as green on any machine without python3 (CLAUDE.md 3).
    echo "run-hook-tests: $py not on PATH — cannot run the hook suites." >&2
    echo "run-hook-tests: this is UNDETERMINED, not a pass." >&2
    exit 1
}

rc=0
for suite in test_precommit_hook test_git_hook_coverage; do
    if [ ! -f "scripts/$suite.py" ]; then
        # A vanished suite must not read as a passing one.
        echo "run-hook-tests: scripts/$suite.py is missing — refusing to report green." >&2
        rc=1
        continue
    fi
    echo "run-hook-tests: running $suite ..." >&2
    if ! "$py" "scripts/$suite.py"; then
        echo "run-hook-tests: $suite FAILED" >&2
        rc=1
    fi
done

exit "$rc"
