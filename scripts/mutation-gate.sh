#!/usr/bin/env bash
# mutation-gate.sh — run cargo-mutants on ONE pilot crate and gate on kill-rate.
#
# WHY: golden/regression tests prove the code still behaves as before; they do not
# prove the tests would CATCH a fault. Mutation testing injects small faults and
# checks the suite fails ("kills" the mutant). The fraction of viable mutants
# killed is the kill-rate / mutation score. See Meta ACH and PRIMG
# (arXiv:2505.05584). This script wires the standard tool `cargo-mutants` to the
# deterministic `mutategate` gate (crates/mutategate), which parses outcomes.json,
# computes the kill-rate, and exits non-zero below the threshold.
#
# SCOPE (deliberately narrow — NOT a silent cut):
#   * PILOT = ONE crate only. `cargo-mutants` across the whole workspace is far
#     too slow to gate on; we start with a single small, pure-logic crate.
#     Default pilot: `harness-core` (shared build-time logic; hash/pricing/spans
#     are pure and well-suited to mutation). Override with PILOT=<crate>.
#   * A second pilot, `specguard`, covers the polarity gate (src/similarity.rs)
#     — a GATE crate whose polarity check has been the source of real bugs
#     found in review, so it is the crate that most needs kill-rate signal.
#     `specguard` is a large crate (~14k lines across main.rs + submodules), so
#     the default MUTANTS_EXTRA for it narrows to src/similarity.rs (the pure,
#     mutation-suited polarity logic) and skips one test
#     (`ack_blocks_when_no_new_commits_since_raised`) that is a known false-flake
#     under cargo-mutants: it reads the real git HEAD via `repo_root()`, but
#     cargo-mutants builds/tests inside a copied scratch tree that is not a git
#     checkout, so `scope::current_head` cannot resolve there. This is a test
#     environment artifact of running outside the real repo, not a mutation
#     finding — excluding it keeps the gate signal instead of flake.
#   * A third pilot, `condukt`, is the first NON-GATE crate added (it is not one
#     of GATE_CRATES: blastguard/propguard/specguard/stuckguard/mutategate/
#     overwatch — see scripts/rollout-plugins.sh). `condukt` is the orchestrator
#     binary (~25k lines across main.rs + submodules) with the highest test
#     density in the workspace (29/36 source files carry tests), so it is a good
#     non-GATE candidate for kill-rate signal. Like specguard, it is far too
#     large to mutate whole, so the default MUTANTS_EXTRA narrows to
#     src/circuit.rs — the circuit-breaker state-machine logic, which is pure
#     (no filesystem/process I/O) and already carries 20 unit tests.
#   * You can narrow further to specific files with
#     MUTANTS_EXTRA="--file crates/harness-core/src/hash.rs" to keep a real run
#     fast. NOTE: `--file` globs are matched against paths relative to the repo
#     root (this script always `cd`s there before invoking cargo-mutants), not
#     relative to the pilot package's own directory.
#     (See the `case "$PILOT"` block below for per-pilot defaults; an
#     explicitly-set MUTANTS_EXTRA overrides the default entirely.)
#
# HOW TO EXPAND (future work):
#   * Add crates one at a time (extend the `case "$PILOT"` block below with a
#     new default scope) once each holds >= threshold, so a newly-added crate
#     cannot silently drag the gate down.
#   * Raise MIN_KILL_RATE as suites harden. Track survivors from mutants.out/.
#
# THRESHOLD: MIN_KILL_RATE default 0.80. Rationale: 0.80 is the practical
# robustness bar used by established mutation tools (e.g. PIT) and the Meta ACH
# line of work; below it a suite is demonstrably missing detectable faults. Kept
# conservative for the pilot so the gate is signal, not flake; raise over time.
#
# TIME: real mutation runs are slow. This script passes --timeout to bound each
# test build+run; tune MUTANTS_TIMEOUT. CI (.github/workflows/mutation.yml) runs
# pilot-limited with an overall job timeout.
#
# USAGE:
#   scripts/mutation-gate.sh                 # pilot=harness-core, threshold=0.80
#   PILOT=difflog MIN_KILL_RATE=0.7 scripts/mutation-gate.sh
#   PILOT=specguard scripts/mutation-gate.sh  # polarity gate (src/similarity.rs)
#   PILOT=condukt scripts/mutation-gate.sh    # circuit-breaker logic (src/circuit.rs)
#   MUTANTS_EXTRA="--file crates/harness-core/src/hash.rs" scripts/mutation-gate.sh
set -euo pipefail

PILOT="${PILOT:-harness-core}"
MIN_KILL_RATE="${MIN_KILL_RATE:-0.80}"
MUTANTS_TIMEOUT="${MUTANTS_TIMEOUT:-120}"

# Per-pilot default scope, used only when the caller does not set MUTANTS_EXTRA.
# specguard is large (~14k lines); narrow to the pure polarity logic
# (src/similarity.rs) and skip the one known git-HEAD-dependent flaky test
# (see comment above) so the gate measures mutation signal, not test-harness
# artifacts of cargo-mutants' scratch-copy execution model.
case "$PILOT" in
  specguard)
    default_mutants_extra="--file crates/specguard/src/similarity.rs --cargo-test-arg=-- --cargo-test-arg=--skip --cargo-test-arg=ack_blocks_when_no_new_commits_since_raised"
    ;;
  condukt)
    default_mutants_extra="--file crates/condukt/src/circuit.rs"
    ;;
  *)
    default_mutants_extra=""
    ;;
esac
MUTANTS_EXTRA="${MUTANTS_EXTRA:-$default_mutants_extra}"

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

# Make cargo available in non-login shells (rustup layout).
if [ -f "$HOME/.cargo/env" ]; then
  # shellcheck disable=SC1091
  . "$HOME/.cargo/env"
fi

if ! cargo mutants --version >/dev/null 2>&1; then
  echo "mutation-gate: cargo-mutants not found." >&2
  echo "  install with: cargo install cargo-mutants --locked" >&2
  exit 2
fi

out_dir="target/mutants-${PILOT}"
# cargo-mutants nests its results under a `mutants.out/` subdirectory of --output.
outcomes="${out_dir}/mutants.out/outcomes.json"

# STALE-RESULT GUARD: cargo-mutants writes into $out_dir but does NOT remove a prior
# run's outcomes.json if THIS run dies before producing its own. Without this clean,
# a run that fails to produce fresh results would fall through to scoring the STALE
# JSON and pass the gate on last run's data. Remove any prior results so the
# "results exist?" check below is truthful: a missing file then means THIS run
# produced nothing => fail-closed. (Under `set -e`, an rm failure aborts the script
# = fail-closed, which is the correct direction — we must not score over a dir we
# could not clean.)
rm -rf "$out_dir"

echo "mutation-gate: running cargo-mutants on pilot crate '${PILOT}' (threshold ${MIN_KILL_RATE})"

# cargo-mutants itself exits non-zero when mutants survive; we want to gate on the
# JSON via mutategate instead, so don't let a survivor abort the script here.
set +e
# shellcheck disable=SC2086
cargo mutants \
  --package "$PILOT" \
  --output "$out_dir" \
  --timeout "$MUTANTS_TIMEOUT" \
  $MUTANTS_EXTRA
mutants_status=$?
set -e

# USE THE EXIT STATUS IN THE DECISION (do not score whatever JSON is on disk).
# Only two statuses mean "a complete mutation run produced a countable result set
# that mutategate should score":
#   0 = success: every viable mutant tested was caught.
#   2 = found surviving/missed mutants: the normal "let the gate judge kill-rate" case.
# EVERY other status means the run did NOT produce a trustworthy result set and must
# fail CLOSED (undetermined) rather than be scored:
#   1  = usage / bad-arguments error (no run happened).
#   4  = baseline (unmutated) build or tests already failing -> no mutant was tested.
#   3  = mutation-induced timeouts/hangs dominated the run (result set not trustworthy).
#   5/6 = --in-diff mismatch / bad diff format.
#   70 = cargo-mutants internal error.
#   *  = interruption/signal, or any code a future cargo-mutants adds.
# Exit codes per https://mutants.rs/exit-codes.html. The catch-all treats unknown
# codes as undetermined = fail-closed, so a newly-added status cannot silently pass
# the gate. (This is deliberately strict: e.g. status 3/timeouts fail-closed rather
# than being scored on a partial run; over-strict is the safe side here and a
# persistent timeout is a --timeout tuning issue, not a reason to wave the gate.)
case "$mutants_status" in
  0 | 2) ;; # complete run -> fall through to score the JSON below
  *)
    echo "mutation-gate: cargo-mutants did not produce a trustworthy result set" >&2
    echo "  (exit status ${mutants_status}: not a completed mutation run -- e.g." >&2
    echo "   baseline failure=4, usage error=1, timeout-dominated=3, internal=70)." >&2
    echo "  Failing CLOSED: an unmeasurable suite is treated as below threshold," >&2
    echo "  never waved through on a stale or partial outcomes.json." >&2
    exit 2
    ;;
esac

if [ ! -f "$outcomes" ]; then
  echo "mutation-gate: expected results at ${outcomes} but none were written" >&2
  echo "  (cargo-mutants exit status was ${mutants_status})" >&2
  exit 2
fi

# The deterministic gate: parse outcomes.json, compute kill-rate, exit 1 if below.
cargo run --quiet --package mutategate -- \
  --outcomes "$outcomes" \
  --min-kill-rate "$MIN_KILL_RATE"
