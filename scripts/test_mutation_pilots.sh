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
run_case "uncalibrated GATE crate only -> default (never empty, no false coverage)" \
  "harness-core" "crates/blastguard/src/main.rs"
run_case "empty input -> default harness-core (never empty)" "harness-core" ""

if [ "$fail" -ne 0 ]; then
  echo "mutation-pilots: TESTS FAILED"
  exit 1
fi
echo "mutation-pilots: all tests passed"
