#!/usr/bin/env python3
"""RED repro (backlog): no way to measure how bad it is when a pre-commit gate breaks.

User direction 2026-11-06: before deciding which gates must run on every commit,
measure for EACH pre-commit gate, by injecting one representative violation:
  (1) fires     — does the gate block that violation now? (false = already dark)
  (2) caught_by — with ONLY that gate disabled, which remaining layers stop the
                  same violation (other pre-commit gates, commit-msg, pre-push,
                  cargo test). Empty = nothing else catches it.
  (3) leak      — if nothing catches it, how far it gets:
                  "local-commit" | "push" | "rollout"; "undetermined" when the
                  probe could not tell (never folded into a benign value).

Contract pinned here for `python3 scripts/gate-severity-probe.py --json`:
  * exit 0, stdout one JSON object {"rev": <sha>, "gates": [...]}.
  * `gates` covers EVERY gate label the hook runs: each `run <scanner> <label>`
    line in .githooks/pre-commit, plus test-weakening (run from commit-msg).
    A gate missing from the report is an unmeasured gate, not a safe one.
    User ruling 2026-11-11 ("Drop it from the test"): main-tree-guard is not
    expected — `condukt guard main-tree` was removed from pre-commit in 432f3a40.
  * each entry: gate (str), fixture (non-empty str: what was injected),
    fires (true | false | "undetermined"), caught_by (list[str]),
    leak: measured with ONLY that gate disabled, independent of `fires`
    (user ruling 2026-11-11, "Follow B and the spec": leak follows invariant
    (3) above). caught_by empty => leak is one of the four values, never null;
    caught_by non-empty => leak is null.
  * anti-vacuity control: gates observed blocking real commits in
    2026-10-08..11-06 transcripts (bump-on-change 60, file-growth 12,
    fail-open-diff 14, secret-guard 5) must report fires == true. A probe that
    reports everything as not firing (or as undetermined) proves nothing.
  * the probe must not touch the caller's working tree or index.

Run: python3 -m pytest scripts/test_gate_severity_probe.py
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parent.parent
_PROBE = _REPO / "scripts" / "gate-severity-probe.py"
_HOOK = _REPO / ".githooks" / "pre-commit"
_LEAKS = {"local-commit", "push", "rollout", "undetermined"}
_KNOWN_FIRING = ("bump-on-change", "file-growth", "fail-open-diff", "secret-guard")


def _expected_gates():
    labels = re.findall(r"^run check-[\w.-]+\.py\s+([\w-]+)", _HOOK.read_text(), re.M)
    # main-tree-guard dropped per user ruling 2026-11-11 (retired from
    # pre-commit in 432f3a40); no retired-gate expectation replaces it.
    return set(labels) | {"test-weakening"}


def _status():
    return subprocess.run(
        ["git", "-C", str(_REPO), "status", "--porcelain"],
        capture_output=True, text=True, check=True,
    ).stdout


class GateSeverityProbe(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.before = _status()
        cls.proc = subprocess.run(
            [sys.executable, str(_PROBE), "--json"],
            cwd=str(_REPO), capture_output=True, text=True, timeout=3600,
        )
        cls.after = _status()

    def _report(self):
        self.assertEqual(self.proc.returncode, 0, self.proc.stderr[-3000:])
        return json.loads(self.proc.stdout)

    def test_covers_every_gate(self):
        got = {g["gate"] for g in self._report()["gates"]}
        missing = _expected_gates() - got
        self.assertFalse(missing, "unmeasured gates: %s" % sorted(missing))

    def test_entry_schema(self):
        for g in self._report()["gates"]:
            self.assertTrue(g.get("fixture"), g)
            self.assertIn(g.get("fires"), (True, False, "undetermined"), g)
            self.assertIsInstance(g.get("caught_by"), list, g)
            # User ruling 2026-11-11 ("Follow B and the spec"): leak is
            # measured with only this gate disabled, so it depends on
            # caught_by alone — a firing gate with no backstop still leaks.
            if g["caught_by"]:
                self.assertIsNone(g.get("leak"), g)
            else:
                self.assertIsNotNone(g.get("leak"), g)
                self.assertIn(g.get("leak"), _LEAKS, g)

    def test_known_firing_gates_fire(self):
        by = {g["gate"]: g for g in self._report()["gates"]}
        for name in _KNOWN_FIRING:
            self.assertIs(by.get(name, {}).get("fires"), True, by.get(name))

    def test_caller_tree_untouched(self):
        self._report()
        self.assertEqual(self.before, self.after)


if __name__ == "__main__":
    unittest.main()
