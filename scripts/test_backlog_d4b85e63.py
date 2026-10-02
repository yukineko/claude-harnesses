#!/usr/bin/env python3
"""backlog d4b85e63: the rollout dry-run canary health gate hides a crashing
`overwatch canary-gate` behind `|| true` and then ALWAYS prints
"[dry-run] gate would PROCEED".

The dry-run branch of run_canary's health gate (scripts/rollout-plugins.sh) is
extracted verbatim — from the `canary-gate --observed-violations 0` line up to
the branch's `else` — and executed under the script's own `set -euo pipefail`
with a stub overwatch whose canary-gate crashes (exit 101). The rollout script
itself is never executed.

Run:  python3 -m unittest scripts.test_backlog_d4b85e63
"""
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROLLOUT = SCRIPTS / "rollout-plugins.sh"


def dry_run_gate_block():
    lines = ROLLOUT.read_text().splitlines()
    starts = [i for i, l in enumerate(lines)
              if "canary-gate --observed-violations 0" in l]
    if len(starts) != 1:
        raise AssertionError(f"expected one dry-run canary-gate call, found {starts}")
    i = starts[0]
    block = []
    for l in lines[i:]:
        if l.strip() == "else":
            break
        block.append(l)
    return "\n".join(block)


def run_block(tmp, gate_body):
    ow = tmp / "ow"
    ow.write_text("#!/usr/bin/env bash\n" + gate_body)
    ow.chmod(0o755)
    h = tmp / "h.sh"
    h.write_text("set -euo pipefail\n"
                 f'ow="{ow}"\ncanary_threshold=1\n' + dry_run_gate_block() + "\n")
    return subprocess.run(["bash", str(h)], capture_output=True, text=True)


class DryRunCanaryGate(unittest.TestCase):
    def test_control_healthy_gate_proceeds(self):
        with tempfile.TemporaryDirectory() as t:
            r = run_block(Path(t), 'echo "PROCEED"\nexit 0\n')
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertIn("would PROCEED", r.stdout)

    @unittest.expectedFailure
    def test_crashing_gate_is_not_reported_as_proceed(self):
        """backlog d4b85e63: open defect (RED observed)."""
        with tempfile.TemporaryDirectory() as t:
            r = run_block(Path(t), 'echo "thread main panicked" >&2\nexit 101\n')
            self.assertNotIn(
                "would PROCEED", r.stdout,
                f"a crashed canary-gate (exit 101) is reported as PROCEED: "
                f"rc={r.returncode} stdout={r.stdout!r} stderr={r.stderr!r}")


if __name__ == "__main__":
    unittest.main()
