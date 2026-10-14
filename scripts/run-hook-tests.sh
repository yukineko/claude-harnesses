#!/bin/sh
# The git-hook test suites, run as one donegate check (backlog
# c3a98510).
#
# These three suites — test_precommit_hook.py, test_git_hook_coverage.py and
# test_gate_bypass.py — exercise the hooks in .githooks/, which are the actual
# block/allow authority in this repo (CLAUDE.md 7: the gate's substance stays
# local, never in CI). Nothing executed any of them. `scripts/check-workspace-
# tests.py` would (it globs scripts/test_*.py), but nothing invokes THAT either,
# so the suites guarding the guards were themselves unguarded.
#
# All three run here as one required check. Measured 2026-09-13 at aed66762,
# test_gate_bypass.py was the red one (failures=1, the amend test pinned by
# backlog 6267bfbe) and ran as a separate ADVISORY check until that red was
# resolved. It was resolved by USER RULING 2026-10-15 (backlog 5d7867f0): the
# amend being recorded is the documented fail-closed design of
# .githooks/pre-commit, and the test was re-anchored to pin that design
# (AmendIsRecordedFailClosed). Measured 2026-10-15: test_gate_bypass.py
# "Ran 148 tests in 143.432s / OK". The advisory entry was deleted in the same
# commit, as its named expiry condition required.
#
# Latency: ~35s for the first two plus ~70-145s for test_gate_bypass, and
# `when_changed` in donegate.toml keeps it off turns
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
for suite in test_precommit_hook test_git_hook_coverage test_gate_bypass; do
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
