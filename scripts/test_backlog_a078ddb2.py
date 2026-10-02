#!/usr/bin/env python3
"""Repro for backlog a078ddb2 (registry monotonicity): a rollout run from a tree
that is OLDER than what another session already registered must not silently
demote the registry pointer.  HERMETIC (HOME/CLAUDE_PLUGIN_* point at a temp dir;
--dry-run, so nothing is written even if the assertion fails).

Currently: the plan emits `registry would update tdd@yukineko: version '99.0.0'
-> '<this tree version>'` - an unconditional last-writer-wins demotion.
"""
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
ROLLOUT = HERE / "rollout-plugins.sh"


class A078ddb2(unittest.TestCase):
    @unittest.expectedFailure  # backlog a078ddb2: open defect; remove when fixed
    def test_rollout_does_not_demote_a_newer_registered_version(self):
        with tempfile.TemporaryDirectory() as t:
            t = Path(t)
            home, cache = t / "home", t / "cache" / "yukineko"
            home.mkdir()
            newer = cache / "tdd" / "99.0.0"
            newer.mkdir(parents=True)
            reg = t / "installed_plugins.json"
            reg.write_text(json.dumps({"version": 1, "plugins": {"tdd@yukineko": [
                {"scope": "user", "installPath": str(newer), "version": "99.0.0"}]}}))
            env = dict(os.environ, HOME=str(home), CLAUDE_PLUGIN_CACHE=str(cache),
                       CLAUDE_PLUGIN_REGISTRY=str(reg),
                       CLAUDE_SETTINGS_JSON=str(home / "settings.json"))
            r = subprocess.run(
                ["bash", str(ROLLOUT), "--dry-run", "--no-rebuild", "--no-sync",
                 "--plugin", "tdd"],
                env=env, capture_output=True, text=True, timeout=300,
            )
            out = r.stdout + r.stderr
            self.assertIn("tdd@yukineko", out, f"fixture did not plan tdd:\n{out}")
            self.assertNotIn(
                "version '99.0.0' ->", out,
                "rollout plans to DEMOTE the registry pointer from a newer "
                f"version registered by another session:\n{out}",
            )


if __name__ == "__main__":
    unittest.main()
