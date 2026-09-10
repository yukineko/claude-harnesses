#!/usr/bin/env bash
# mutation-pilots.sh — map a list of changed file paths (stdin, one per line) to
# the calibrated mutation-gate pilot crate(s) to run, one per line on stdout.
#
# WHY: the mutation gate (scripts/mutation-gate.sh + crates/mutategate) is the only
# gate that measures whether the existing test suite would CATCH a fault (golden /
# regression CI only proves behaviour is unchanged). It was previously wired to fire
# ONLY when its own machinery changed (crates/mutategate/**, this script's sibling,
# the workflow file), so a change to a pilot crate's implementation+tests never ran
# it — the gate was INERT against its actual judgment target (100% passthrough).
# That is a fail-open: donegate's required test-changed check is green whenever the
# tests still pass, and the ONE gate that can tell "implementation improved" from
# "tests weakened" was never fired at the crate that changed. This helper is the
# wiring: given the files a PR/push touched, it names which pilot crate(s) to
# mutate, so the gate fires against the crate that actually changed and narrows the
# (slow) cost to only that crate.
#
# FAIL-CLOSED: this helper NEVER prints an empty list. If the changed set matches no
# calibrated pilot (only the gate machinery changed, or an as-yet-uncalibrated crate
# changed) it falls back to the default pilot `harness-core` so the gate still runs
# against SOMETHING. Emitting nothing would re-introduce the exact inert-gate
# fail-open this wiring exists to close.
#
# CALIBRATED PILOTS: keep this list in lockstep with the `case "$PILOT"` block in
# scripts/mutation-gate.sh. A crate only belongs here once it has a calibrated scope
# there and holds >= MIN_KILL_RATE (see "HOW TO EXPAND" in mutation-gate.sh). The
# remaining GATE_CRATES were MEASURED on 2026-09-11 at 89b31bb4 (cargo-mutants, one
# pure file each, threshold 0.80). `blastguard` qualified and was added; the other
# three did NOT, so they stay deliberately absent:
#
#   blastguard  src/classify.rs   95.0%  (19/20 viable)            ADDED
#   stuckguard  src/detect.rs     78.8%  (41/52, 11 survived)      below bar
#   propguard   src/config.rs     48.7%  (19/39, 20 survived)      below bar
#   overwatch   src/canary.rs     UNDETERMINED (baseline failure)  unmeasurable
#
# Re-measure with: PILOT=<crate> MUTANTS_EXTRA="--file <path>" scripts/mutation-gate.sh
# These three absences are a REPORTED measurement, not an untried backlog item.
# Listing a crate whose suite is below the bar would be false coverage — a fail-open
# of its own — and listing overwatch, whose baseline `cargo test` fails inside
# cargo-mutants' scratch copy (exit status 4: no mutant was ever tested), would score
# a run that measured nothing. Raising those suites is tracked follow-up work.
set -euo pipefail

# Priority order = the order pilots are emitted when several changed. Also the
# fallback default (first entry) when nothing calibrated matched.
PILOTS="harness-core specguard condukt blastguard"
DEFAULT_PILOT="harness-core"

changed="$(cat)"

emitted=""
for pilot in $PILOTS; do
  # Any changed path under crates/<pilot>/ means that pilot must be mutated.
  if printf '%s\n' "$changed" | grep -q "^crates/${pilot}/"; then
    printf '%s\n' "$pilot"
    emitted="yes"
  fi
done

# Fail-closed fallback: a change that triggered the gate but touched no calibrated
# pilot (machinery-only, or an uncalibrated crate on a manual run) still runs the
# default pilot. NEVER emit an empty list — that is the inert-gate passthrough.
if [ -z "$emitted" ]; then
  printf '%s\n' "$DEFAULT_PILOT"
fi
