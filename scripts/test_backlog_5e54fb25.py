#!/usr/bin/env python3
"""Repro for backlog 5e54fb25: scripts/rollout-plugins.sh run with --plugin
skips its post-rollout verify and returns 0, so drift that remains after the
run reads as success. check-plugin-rollout.py's own fix hint tells the operator
to run exactly that form ("fix with: scripts/rollout-plugins.sh --plugin tdd").

Measured without running a rollout (which would mutate the real ~/.claude):
the test lifts the verify_rollout_complete() function verbatim out of the
script and calls it in a bash sandbox where REPO points at a fixture whose
scripts/check-plugin-rollout.py reports drift for the filtered plugin (exit 1,
the ROLLOUT class) whatever arguments it is given.

  control: unfiltered run + drift -> non-zero (the script's whole-fleet path)
  repro:   --plugin tdd   + drift -> must also be non-zero; today it is 0
"""
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "rollout-plugins.sh"


def verify_fn() -> str:
    src = SCRIPT.read_text(encoding="utf-8")
    m = re.search(r"^verify_rollout_complete\(\) \{\n.*?^\}\n", src, re.S | re.M)
    if not m:
        raise AssertionError("verify_rollout_complete() not found -- undetermined, not clean")
    return m.group(0)


def call(only_plugins: str) -> tuple[int, str]:
    with tempfile.TemporaryDirectory() as t:
        (Path(t) / "scripts").mkdir()
        (Path(t) / "scripts" / "check-plugin-rollout.py").write_text(
            "import sys\nprint('ROLLOUT DRIFT: tdd: deployed tree is not a mirror of crates/tdd')\nsys.exit(1)\n")
        prog = (f"set -u\nREPO='{t}'\ndry=0\nonly_plugins=({only_plugins})\n"
                + verify_fn() + "\nverify_rollout_complete\necho \"rc=$?\"\n")
        r = subprocess.run(["bash", "-c", prog], capture_output=True, text=True)
        out = r.stdout + r.stderr
        m = re.search(r"rc=(\d+)", r.stdout)
        if not m:
            raise AssertionError(f"harness did not report rc -- undetermined:\n{out}")
        return int(m.group(1)), out


class FilteredRolloutVerify(unittest.TestCase):
    def test_control_unfiltered_run_with_drift_fails(self):
        rc, out = call("")
        self.assertNotEqual(rc, 0, out)
        self.assertIn("drift remains", out)

    @unittest.expectedFailure  # backlog 5e54fb25: open defect, remove when fixed
    def test_filtered_run_with_drift_in_the_filtered_plugin_fails(self):
        rc, out = call("tdd")
        self.assertNotEqual(rc, 0, f"--plugin tdd returned 0 while tdd drift remains:\n{out}")


if __name__ == "__main__":
    unittest.main()
