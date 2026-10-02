#!/usr/bin/env python3
"""RED repro for backlog acf4f7ba.

flow SKILL.md Step 3-1 (3) runs `compass gap` and then says to adopt "that move
as `to_condukt`". `to_condukt` is not a field of `compass gap`'s output; it is
`RouteResult.to_condukt` of the separate `compass route` subcommand
(crates/compass/src/route.rs), which the flow SKILL never invokes. A literal
reader gets None and can misread it as "no move can be drawn = charter stale",
which is one of Step 1's stop conditions.

The test runs the real `compass gap` (built from this checkout) in a throwaway
git repo seeded with this repo's charter, and asserts: if the SKILL tells the
reader to take `to_condukt` right after `compass gap`, then that key exists in
the gap JSON (or the SKILL invokes `compass route`). Open defect ->
expectedFailure.

    python3 scripts/test_backlog_acf4f7ba.py
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parent.parent
SKILL = _REPO / "crates/flow/skills/flow/SKILL.md"
CHARTER = _REPO / ".compass/charter.md"


def _compass_bin() -> str:
    cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
    env = dict(os.environ)
    env.setdefault("CARGO_BUILD_JOBS", "2")
    subprocess.run([cargo, "build", "-q", "-p", "compass"], cwd=_REPO, env=env,
                   check=True)
    meta = subprocess.run([cargo, "metadata", "--format-version=1", "--no-deps"],
                          cwd=_REPO, capture_output=True, text=True, check=True)
    target = json.loads(meta.stdout)["target_directory"]
    return os.path.join(target, "debug", "compass")


class SkillGapFieldMatchesGapOutput(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="acf4f7ba.")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        os.makedirs(os.path.join(self.tmp, ".compass"))
        shutil.copy(CHARTER, os.path.join(self.tmp, ".compass", "charter.md"))
        for args in (["init", "-q"], ["config", "user.email", "t@t"],
                     ["config", "user.name", "t"], ["add", "-A"],
                     ["commit", "-qm", "init"]):
            subprocess.run(["git", *args], cwd=self.tmp, check=True,
                           capture_output=True)

    @unittest.expectedFailure  # backlog acf4f7ba: open defect, remove when fixed
    def test_to_condukt_referenced_after_gap_exists_in_gap_output(self):
        text = SKILL.read_text()
        start = text.index("compass gap     #")
        step = text[start:start + 1200]
        # Precondition: the SKILL step really does name `to_condukt`.
        self.assertIn("`to_condukt`", step)
        p = subprocess.run([_compass_bin(), "gap"], cwd=self.tmp,
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        keys = sorted(json.loads(p.stdout).keys())
        self.assertTrue(keys, "anti-vacuity: gap produced keys")
        self.assertTrue(
            "to_condukt" in keys or "compass route" in step,
            "SKILL adopts `to_condukt` after `compass gap`, but gap's keys are %r "
            "and the step never calls `compass route`" % keys,
        )


if __name__ == "__main__":
    unittest.main()
