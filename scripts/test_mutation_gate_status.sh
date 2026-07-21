#!/usr/bin/env bash
# Tests that mutation-gate.sh USES cargo-mutants' exit status and never scores a
# stale/leftover outcomes.json. cargo-mutants is stubbed via a PATH shim so this
# runs without the real (slow, uninstalled) engine.
#
# The load-bearing assertions (the fail-open being closed):
#   * a non-completion exit status (baseline failure=4, usage=1, timeout=3,
#     internal=70, or any unknown) must FAIL CLOSED (rc 2) and NOT score, even when
#     an outcomes.json is present on disk;
#   * only statuses {0, 2} proceed to score;
#   * a STALE outcomes.json from a prior run must be removed before this run, so a
#     run that produces nothing cannot be scored on old data.
set -uo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo_root="$(cd "$here/.." && pwd)"
# Overridable so a teeth-check / verifier can point it at a reverted copy of the
# gate (kept in scripts/ so the gate's own BASH_SOURCE/../ still resolves the repo).
gate="${GATE_UNDER_TEST:-$here/mutation-gate.sh}"
fail=0

shim_dir="$(mktemp -d "${TMPDIR:-/tmp}/mutgate-shim.XXXXXX")"
fake_home="$shim_dir/home"
mkdir -p "$fake_home"
# Clean the shared target dir the gate uses, both up front and on exit.
trap 'rm -rf "$shim_dir" "$repo_root/target/mutants-harness-core"' EXIT

# Fake `cargo`:
#   `cargo mutants --version`  -> exit 0 (pretend cargo-mutants is installed)
#   `cargo mutants ... --output D ...` -> optionally write D/mutants.out/outcomes.json,
#                                         then exit $STUB_MUTANTS_STATUS
#   `cargo run ... mutategate` -> exit $STUB_SCORE_STATUS (default 0 = "kill-rate ok")
cat > "$shim_dir/cargo" <<'SHIM'
#!/usr/bin/env bash
if [ "${1:-}" = "mutants" ] && [ "${2:-}" = "--version" ]; then
  echo "cargo-mutants 99.99.99 (stub)"; exit 0
fi
if [ "${1:-}" = "mutants" ]; then
  out=""
  while [ $# -gt 0 ]; do
    if [ "$1" = "--output" ]; then out="${2:-}"; fi
    shift
  done
  if [ "${STUB_WRITE_OUTCOMES:-1}" = "1" ] && [ -n "$out" ]; then
    mkdir -p "$out/mutants.out"
    printf '%s' '{"outcomes":[]}' > "$out/mutants.out/outcomes.json"
  fi
  exit "${STUB_MUTANTS_STATUS:-0}"
fi
if [ "${1:-}" = "run" ]; then
  exit "${STUB_SCORE_STATUS:-0}"
fi
exit 0
SHIM
chmod +x "$shim_dir/cargo"

# Run mutation-gate.sh under the shim; echo its exit code. HOME override stops the
# gate from sourcing the real ~/.cargo/env (which would re-prepend the real cargo
# ahead of our shim). PILOT/MUTANTS_EXTRA are arbitrary — the shim ignores them.
run_gate() {
  (
    cd "$repo_root"
    HOME="$fake_home" PATH="$shim_dir:$PATH" \
      PILOT=harness-core MUTANTS_EXTRA="--file crates/harness-core/src/hash.rs" \
      bash "$gate" >/dev/null 2>&1
    echo $?
  )
}

check() { # name want got
  if [ "$3" != "$2" ]; then
    echo "FAIL: $1 (want rc=$2 got rc=$3)"; fail=1
  else
    echo "ok: $1"
  fi
}

# {0,2} -> a complete run is scored; the scorer's verdict propagates.
check "status 0 -> score, scorer pass -> rc 0" 0 \
  "$(STUB_MUTANTS_STATUS=0 STUB_SCORE_STATUS=0 run_gate)"
check "status 2 (survivors) -> score, scorer fail -> rc 1" 1 \
  "$(STUB_MUTANTS_STATUS=2 STUB_SCORE_STATUS=1 run_gate)"

# Non-completion statuses fail CLOSED (rc 2) BEFORE scoring, even though the stub
# wrote an outcomes.json — the status is trusted over the file's mere presence.
check "status 4 baseline-fail -> fail-closed rc 2 (not scored)" 2 \
  "$(STUB_MUTANTS_STATUS=4 run_gate)"
check "status 1 usage -> fail-closed rc 2" 2 \
  "$(STUB_MUTANTS_STATUS=1 run_gate)"
check "status 3 timeout -> fail-closed rc 2 (conservative)" 2 \
  "$(STUB_MUTANTS_STATUS=3 run_gate)"
check "status 70 internal -> fail-closed rc 2" 2 \
  "$(STUB_MUTANTS_STATUS=70 run_gate)"
check "unknown future status 99 -> fail-closed rc 2 (catch-all)" 2 \
  "$(STUB_MUTANTS_STATUS=99 run_gate)"

# STALE GUARD: pre-seed a leftover outcomes.json, then have THIS run produce nothing
# and exit 0. The pre-clean must delete the stale file so scoring cannot run on old
# data -> missing-file fail-closed (rc 2). Without the `rm -rf "$out_dir"` fix the
# stale file survives and gets scored (rc 0) -> this case goes RED.
mkdir -p "$repo_root/target/mutants-harness-core/mutants.out"
printf '%s' '{"stale":true}' \
  > "$repo_root/target/mutants-harness-core/mutants.out/outcomes.json"
check "stale outcomes + this-run-writes-nothing -> pre-clean -> fail-closed rc 2" 2 \
  "$(STUB_MUTANTS_STATUS=0 STUB_WRITE_OUTCOMES=0 run_gate)"

if [ "$fail" -ne 0 ]; then
  echo "mutation-gate-status: TESTS FAILED"
  exit 1
fi
echo "mutation-gate-status: all tests passed"
