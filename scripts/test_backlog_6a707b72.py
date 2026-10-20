#!/usr/bin/env python3
"""Backlog 6a707b72 probe: does a FILTERED rollout (--plugin X) repoint/create
version dirs for plugins OUTSIDE the filter?

HERMETIC (HOME/CLAUDE_PLUGIN_CACHE/CLAUDE_PLUGIN_REGISTRY/CLAUDE_SETTINGS_JSON in
a temp dir). Uses --no-rebuild --no-sync, so this covers the copy+registry
stage only; the rebuild/seed stage is NOT covered (it takes --only=<names>).

Run: python3 scripts/test_backlog_6a707b72.py
"""
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
ROLLOUT = HERE / "rollout-plugins.sh"

TARGET = "taskprog"
BYSTANDER = "hypothesis"


class FilteredRolloutLeavesOthersAlone(unittest.TestCase):
    def test_bystander_registry_and_cache_untouched(self):
        with tempfile.TemporaryDirectory() as tmp:
            tmp = Path(tmp)
            home, cache = tmp / "home", tmp / "cache" / "yukineko"
            reg = tmp / "installed_plugins.json"
            home.mkdir()
            entries = {}
            for n in (TARGET, BYSTANDER):
                d = cache / n / "0.0.1-prior"
                d.mkdir(parents=True)
                (d / "marker").write_text("prior")
                entries[f"{n}@yukineko"] = [
                    {"scope": "user", "installPath": str(d), "version": "0.0.1-prior"}
                ]
            reg.write_text(json.dumps({"version": 1, "plugins": entries}))
            before_reg = json.loads(reg.read_text())["plugins"][f"{BYSTANDER}@yukineko"]
            before_tree = sorted(
                str(p.relative_to(cache)) for p in (cache / BYSTANDER).rglob("*")
            )
            env = dict(os.environ)
            env.update(
                HOME=str(home),
                CLAUDE_PLUGIN_CACHE=str(cache),
                CLAUDE_PLUGIN_REGISTRY=str(reg),
                CLAUDE_SETTINGS_JSON=str(home / "settings.json"),
            )
            r = subprocess.run(
                ["bash", str(ROLLOUT), "--plugin", TARGET, "--no-rebuild", "--no-sync"],
                env=env, capture_output=True, text=True, cwd=str(REPO),
            )
            log = r.stdout + r.stderr
            # c9373b92 contract: a filtered rollout verifies the TARGETED plugin.
            # --no-rebuild leaves taskprog deployed dark (no binary), so the run
            # must fail on exactly that, and must report the bystander's drift
            # as out of scope (not fatal, not enforced) rather than fail on it.
            self.assertEqual(r.returncode, 1, log)
            self.assertRegex(
                log, rf"{TARGET}: crates/{TARGET} declares a binary target.*execs nothing", log
            )
            self.assertIn("OUT OF SCOPE", log)
            oos = log.split("OUT OF SCOPE", 1)[1]
            self.assertRegex(oos, rf"- {BYSTANDER}: ", log)
            after = json.loads(reg.read_text())["plugins"]
            # control arm: the targeted plugin WAS repointed (the run did something)
            self.assertNotEqual(
                after[f"{TARGET}@yukineko"][0]["version"], "0.0.1-prior", log
            )
            self.assertEqual(after[f"{BYSTANDER}@yukineko"], before_reg, log)
            after_tree = sorted(
                str(p.relative_to(cache)) for p in (cache / BYSTANDER).rglob("*")
            )
            self.assertEqual(after_tree, before_tree, log)


if __name__ == "__main__":
    unittest.main()
