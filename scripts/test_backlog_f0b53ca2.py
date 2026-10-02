"""Repro for backlog f0b53ca2: record-audit.py says "exceeds threshold" for a breach
of a FLOOR-type dimension.

`audit-convergence` breaches when value < 1 (collect()), but escalate() renders every
breach with one template, "<key> at <value> exceeds threshold <threshold>". A value of
0 against a floor of 1 is therefore reported to the review queue as "0 exceeds
threshold 1" -- the summary states the opposite of the measured fact.

The subject is loaded with exec(compile()) rather than a SourceFileLoader shim so a
stale __pycache__ cannot stand in for the file (backlog 05726f9f). No subprocess is
run: `_run` and `already_open` are replaced to capture what would be recorded.
"""
import sys
import types
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SUBJECT = HERE / "record-audit.py"


def load():
    mod = types.ModuleType("record_audit_under_test")
    mod.__file__ = str(SUBJECT)
    sys.path.insert(0, str(HERE))
    try:
        exec(compile(SUBJECT.read_text(encoding="utf-8"), str(SUBJECT), "exec"), mod.__dict__)
    finally:
        sys.path.remove(str(HERE))
    return mod


class BacklogF0b53ca2(unittest.TestCase):
    def setUp(self):
        self.ra = load()
        self.calls = []

        def fake_run(cmd, cwd=None, timeout=180):
            self.calls.append(cmd)
            return 0, "", ""

        self.ra._run = fake_run
        self.ra.already_open = lambda _fid: False

    def summary_for(self, dim):
        self.ra.escalate([dim], dry_run=False)
        self.assertEqual(len(self.calls), 1, self.calls)
        cmd = self.calls[0]
        return cmd[cmd.index("--summary") + 1]

    def test_control_ceiling_breach_says_exceeds(self):
        d = self.ra.Dimension("review-queue-depth", "t", self.ra.Measurement.known(9), 5, True, "med")
        self.assertIn("exceeds", self.summary_for(d))

    @unittest.expectedFailure  # backlog f0b53ca2: open defect, remove when fixed
    def test_floor_breach_is_not_reported_as_exceeding(self):
        # Same construction as collect() uses for audit-convergence: threshold 1, breach when < 1.
        m = self.ra.Measurement.known(0)
        d = self.ra.Dimension("audit-convergence", "t", m, 1, m.is_known and m.value < 1, "high")
        self.assertEqual(d.state, "breach")
        summary = self.summary_for(d)
        self.assertNotIn("exceeds", summary, summary)


if __name__ == "__main__":
    unittest.main()
