#!/usr/bin/env bash
# Tests for mutation-pilots.sh — the changed-files -> pilot-crate mapping that wires
# the mutation gate to its judgment target. The load-bearing assertion is the
# FAIL-CLOSED invariant: the helper never emits an empty pilot list (an empty list
# would run the gate against nothing = the inert-gate passthrough this closes).
# Run: scripts/test_mutation_pilots.sh
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
helper="$here/mutation-pilots.sh"
fail=0

# run_case <name> <expected-newline-list> <stdin>
run_case() {
  name="$1"; expected="$2"; input="$3"
  got="$(printf '%s' "$input" | "$helper")"
  if [ "$got" != "$expected" ]; then
    echo "FAIL: $name"
    echo "  input:    [$(printf '%s' "$input" | tr '\n' ',')]"
    echo "  expected: [$(printf '%s' "$expected" | tr '\n' ',')]"
    echo "  got:      [$(printf '%s' "$got" | tr '\n' ',')]"
    fail=1
  else
    echo "ok: $name"
  fi
}

# A change under a calibrated pilot fires the gate against THAT pilot (the whole
# point: the gate now reaches its judgment target, scoped to the changed crate).
run_case "specguard change -> specguard" "specguard" \
  "crates/specguard/src/similarity.rs"
run_case "condukt change -> condukt" "condukt" \
  "crates/condukt/src/circuit.rs"
run_case "harness-core change -> harness-core" "harness-core" \
  "crates/harness-core/src/hash.rs"

# Several calibrated pilots changed -> each runs, in the helper's priority order.
run_case "harness-core+specguard -> both (priority order)" \
  "$(printf 'harness-core\nspecguard')" \
  "$(printf 'crates/specguard/src/similarity.rs\ncrates/harness-core/src/pricing.rs')"

# A path that merely CONTAINS a pilot name but is not under crates/<pilot>/ must
# not match (anchor discipline — avoids firing on unrelated files).
run_case "non-crate path mentioning pilot -> default (no spurious match)" \
  "harness-core" "docs/harness-core-notes.md"

# THE FAIL-CLOSED INVARIANT: a triggered run that touched no calibrated pilot still
# runs the default pilot, NEVER an empty list.
run_case "machinery-only change -> default harness-core" "harness-core" \
  "scripts/mutation-gate.sh"
# NOTE ON THE ANCHOR: this case used to use blastguard as its stand-in for "an
# uncalibrated GATE crate". blastguard was CALIBRATED on 2026-09-11 (classify.rs,
# 95.0% kill-rate) and added to PILOTS, so that proxy drifted and the case started
# failing. The named property — an UNCALIBRATED crate must fall back to the default
# rather than claim coverage it has not earned — is unchanged, so the case is
# re-anchored on a crate that is still measurably uncalibrated (propguard,
# measured 48.7% on src/config.rs) rather than deleted or relaxed. If propguard is
# ever calibrated, re-anchor again; do NOT change the expectation to match.
run_case "uncalibrated GATE crate only -> default (never empty, no false coverage)" \
  "harness-core" "crates/propguard/src/main.rs"

# blastguard is a calibrated pilot as of 2026-09-11 — a change under it must now
# fire the gate against blastguard itself, not fall back to the default. This is
# the positive half of the case above: together they pin that the fallback is
# driven by CALIBRATION, not by a crate being a GATE crate.
run_case "calibrated GATE crate (blastguard) -> blastguard" "blastguard" \
  "crates/blastguard/src/classify.rs"
run_case "empty input -> default harness-core (never empty)" "harness-core" ""

if [ "$fail" -ne 0 ]; then
  echo "mutation-pilots: TESTS FAILED"
  exit 1
fi
echo "mutation-pilots: all tests passed"
