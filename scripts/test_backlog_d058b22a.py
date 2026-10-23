"""backlog d058b22a: scripts/mutation-gate.sh validates MIN_KILL_RATE only
AFTER the full (expensive) cargo-mutants run. A threshold mutategate rejects
(MIN_KILL_RATE=0: "--min-kill-rate must be greater than 1e-9 ...", exit 2) is
only discovered once `cargo mutants --package ...` has already run, because
the script hands the threshold to `cargo run -p mutategate` as its LAST step.

The real engine is replaced by a PATH shim `cargo` that LOGS every invocation,
so the test observes the ORDER: was `cargo mutants --package` invoked before
the invalid threshold was rejected? HOME is overridden so the gate does not
source the real ~/.cargo/env (which would re-prepend the real cargo).
"""

import os
import shutil
import subprocess
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(HERE)
GATE = os.path.join(HERE, "mutation-gate.sh")
PILOT = "backlog-d058b22a-probe"

SHIM = """#!/usr/bin/env bash
echo "$*" >> "$SHIM_LOG"
if [ "${1:-}" = "mutants" ] && [ "${2:-}" = "--version" ]; then
  echo "cargo-mutants 99.99.99 (stub)"; exit 0
fi
if [ "${1:-}" = "mutants" ]; then
  out=""
  while [ $# -gt 0 ]; do
    if [ "$1" = "--output" ]; then out="${2:-}"; fi
    shift
  done
  mkdir -p "$out/mutants.out"
  printf '%s' '{"outcomes":[]}' > "$out/mutants.out/outcomes.json"
  exit 0
fi
if [ "${1:-}" = "run" ]; then
  # Stand-in for mutategate's threshold rejection (observed real message/rc).
  echo "mutategate: --min-kill-rate must be greater than 1e-9" >&2
  exit 2
fi
exit 0
"""


def run_gate(min_kill_rate):
    tmp = tempfile.mkdtemp(prefix="mutgate-d058b22a-")
    try:
        home = os.path.join(tmp, "home")
        os.makedirs(home)
        cargo = os.path.join(tmp, "cargo")
        with open(cargo, "w") as f:
            f.write(SHIM)
        os.chmod(cargo, 0o755)
        log = os.path.join(tmp, "cargo.log")
        env = dict(os.environ)
        env.update(
            HOME=home,
            PATH=tmp + os.pathsep + "/usr/bin:/bin",
            SHIM_LOG=log,
            PILOT=PILOT,
            MUTANTS_EXTRA="",
            MIN_KILL_RATE=min_kill_rate,
        )
        p = subprocess.run(["bash", GATE], env=env, capture_output=True, text=True)
        calls = open(log).read().splitlines() if os.path.exists(log) else []
        return p.returncode, calls, p.stdout + p.stderr
    finally:
        shutil.rmtree(tmp, ignore_errors=True)
        shutil.rmtree(os.path.join(REPO, "target", "mutants-" + PILOT), ignore_errors=True)


def ran_mutants(calls):
    return any(c.startswith("mutants ") and "--package" in c for c in calls)


class MutationGateThresholdOrder(unittest.TestCase):
    def test_control_valid_threshold_runs_cargo_mutants(self):
        rc, calls, out = run_gate("0.80")
        self.assertTrue(ran_mutants(calls), "control: shim log must record the run: %r" % calls)

    def test_invalid_threshold_is_rejected_before_cargo_mutants_runs(self):
        rc, calls, out = run_gate("0")
        self.assertNotEqual(rc, 0, out)
        self.assertFalse(
            ran_mutants(calls),
            "MIN_KILL_RATE=0 is a threshold mutategate always rejects, yet the gate ran "
            "the full `cargo mutants --package` first and only rejected afterwards. "
            "cargo calls: %r" % calls,
        )


if __name__ == "__main__":
    unittest.main()
