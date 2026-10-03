#!/bin/bash
# Runs the 18fe626f per-version prune hold suites (stdlib unittest; no pytest needed).
# Prints `<N> passed` on success so the closure-evidence runner can see tests ran.
set -u
cd "$(dirname "$0")/../.."
out=$(python3 -m unittest scripts.test_prune_version_history scripts.test_prune_version_history_extras 2>&1)
rc=$?
printf '%s\n' "$out"
n=$(printf '%s\n' "$out" | sed -n 's/^Ran \([0-9][0-9]*\) tests\{0,1\} in .*/\1/p' | tail -1)
if [ "$rc" -eq 0 ] && [ -n "$n" ]; then echo "$n passed"; fi
exit "$rc"
