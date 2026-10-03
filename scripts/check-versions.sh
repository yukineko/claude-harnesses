#!/usr/bin/env bash
# Verify every crate's [package] version in Cargo.toml matches the version in its
# .claude-plugin/plugin.json. The plugin.json is what `/plugin install` ships, so
# the two must agree or the build metadata and the distributed manifest drift.
#
# Exit 0 when all match; exit 1 listing every mismatch otherwise. Inspecting
# zero crates is NOT a match: a missing crates/ dir (or a failed cd) used to leave
# the glob unexpanded, skip every iteration and print "OK" (backlog f6919056).
set -uo pipefail
shopt -s nullglob

cd "$(dirname "$0")/.." || { echo "version parity check FAILED — cannot cd to the repo root"; exit 1; }

fail=0
checked=0
for d in crates/*/; do
  ct="$d/Cargo.toml"
  pj="$d/.claude-plugin/plugin.json"
  # Only crates that are BOTH a Cargo crate and a plugin are in scope.
  [ -f "$ct" ] && [ -f "$pj" ] || continue
  checked=$((checked + 1))

  cv=$(grep -m1 '^version' "$ct" | sed -E 's/.*"([^"]+)".*/\1/')
  pv=$(grep -m1 '"version"' "$pj" | sed -E 's/.*"version"[: ]+"([^"]+)".*/\1/')

  if [ -z "$cv" ] || [ -z "$pv" ]; then
    echo "WARN  $(basename "$d"): could not read version (cargo='$cv' plugin='$pv')"
    fail=1
    continue
  fi
  if [ "$cv" != "$pv" ]; then
    echo "MISMATCH $(basename "$d"): Cargo.toml=$cv plugin.json=$pv"
    fail=1
  fi
done

if [ "$checked" -eq 0 ]; then
  echo "version parity check FAILED — no crate with both Cargo.toml and plugin.json was found; nothing was verified."
  exit 1
fi

if [ "$fail" -ne 0 ]; then
  echo "version parity check FAILED — bump Cargo.toml and plugin.json together."
  exit 1
fi
echo "version parity check OK"
