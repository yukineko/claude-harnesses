#!/usr/bin/env bash
# Sync every plugin.json version to its crate's Cargo.toml version — the
# WRITE companion to check-plugin-versions.py (which only *verifies* parity,
# read-only, and also compares .claude-plugin/marketplace.json).
#
# The Cargo.toml [package] version is the source of truth; each
# .claude-plugin/plugin.json is rewritten to match. Run this after a tool bumps
# Cargo.toml (e.g. release-plz) so the distributed manifest (plugin.json) stays
# in lockstep and the version-lockstep gate (check-plugin-versions.py, run by
# .githooks/pre-commit) stays green. marketplace.json is NOT touched here.
#
# Idempotent: a crate already in sync is skipped with no file write, so running
# it repeatedly (or when nothing changed) is a clean no-op. A crate that is not
# BOTH a cargo crate and a plugin is skipped.
# Exit 0 always — this is a fix-up step, not a gate; use
# check-plugin-versions.py to assert parity.
#
# Portable across GNU (CI/ubuntu) and BSD (macOS) userlands: the in-place edit
# uses awk match()/substr() rather than sed's non-portable `0,/re/` address, and
# replaces only the FIRST "version" line so nested "version" keys are untouched.
set -uo pipefail

cd "$(dirname "$0")/.."

changed=0
for d in crates/*/; do
  ct="$d/Cargo.toml"
  pj="$d/.claude-plugin/plugin.json"
  # Only crates that are BOTH a cargo crate and a plugin are in scope.
  [ -f "$ct" ] && [ -f "$pj" ] || continue

  cv=$(grep -m1 '^version' "$ct" | sed -E 's/.*"([^"]+)".*/\1/')
  pv=$(grep -m1 '"version"' "$pj" | sed -E 's/.*"version"[: ]+"([^"]+)".*/\1/')

  if [ -z "$cv" ]; then
    echo "WARN  $(basename "$d"): could not read Cargo.toml version; skipped"
    continue
  fi
  [ "$cv" = "$pv" ] && continue

  # Rewrite only the first `"version": "..."` line, preserving that line's
  # indentation and any trailing characters (e.g. the comma).
  tmp="$pj.tmp.$$"
  if awk -v cv="$cv" '
      !done && match($0, /"version"[ \t]*:[ \t]*"[^"]*"/) {
        $0 = substr($0, 1, RSTART - 1) "\"version\": \"" cv "\"" substr($0, RSTART + RLENGTH)
        done = 1
      }
      { print }
    ' "$pj" > "$tmp"; then
    mv "$tmp" "$pj"
    echo "synced $(basename "$d"): plugin.json $pv -> $cv"
    changed=1
  else
    rm -f "$tmp"
    echo "WARN  $(basename "$d"): rewrite failed; left unchanged"
  fi
done

if [ "$changed" -eq 0 ]; then
  echo "all plugin.json versions already in sync"
fi
exit 0
