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
the gap JSON (or the SKILL invokes `compass route`); more generally every gap
field the step names in backticks must be a key of the gap JSON. The SKILL path
can be redirected with FLOW_SKILL_MD (RED check against the pre-fix prose).

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
SKILL = Path(os.environ.get("FLOW_SKILL_MD", _REPO / "crates/flow/skills/flow/SKILL.md"))
GAP_FIELDS = ("opportunities", "dod", "measuring_stick", "last_outcome",
              "to_condukt", "north_star", "current_gap")
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

    def test_gap_fields_named_after_gap_exist_in_gap_output(self):
        text = SKILL.read_text()
        start = text.index("compass gap     #")
        end = text.find("\n4. ", start)
        step = text[start:end if end != -1 else start + 3000]
        named = {f for f in GAP_FIELDS if f"`{f}`" in step}
        # Anti-vacuity: the step must name at least one gap field, else the
        # check below proves nothing about the prose.
        self.assertTrue(named, "step names no gap field in backticks")
        p = subprocess.run([_compass_bin(), "gap"], cwd=self.tmp,
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stderr)
        keys = set(json.loads(p.stdout).keys())
        self.assertTrue(keys, "anti-vacuity: gap produced keys")
        missing = sorted(named - keys)
        if "to_condukt" in missing and "compass route" in step:
            missing.remove("to_condukt")  # produced by `compass route`, which the step invokes
        self.assertEqual(
            missing, [],
            "SKILL names gap fields that `compass gap` does not output: %r (gap keys: %r)"
            % (missing, sorted(keys)),
        )


if __name__ == "__main__":
    unittest.main()
