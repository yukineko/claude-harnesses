#!/usr/bin/env python3
"""Repro for backlog e8aad6e6: check-plugin-rollout.py reports a deployed binary
that links a NEWER harness-core than the source (orphan provenance; rollout would
be a destructive rollback) with the same message/remedy as one that is OLDER.

Fixed (both merge parents implemented the direction split); the skip was removed
in the merge that integrated them, so this repro now guards the fix."""
import importlib.util
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "test_check_plugin_rollout", _HERE / "test_check_plugin_rollout.py"
)
base = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(base)


class DeployedNewerThanSource(base._FixtureCase):
    def test_deployed_core_newer_than_source_is_not_prescribed_a_rollout(self):
        with tempfile.TemporaryDirectory() as tmp:
            rc, out, err = self.run_main(
                tmp,
                provenance={
                    "condukt": {
                        "commit": "deadbeef" * 5,
                        "dirty": False,
                        "harness_core_version": "0.2.3",  # deployed is NEWER
                    }
                },
                core_version="0.2.2",  # source is older
            )
        text = (out + err).lower()
        self.assertNotEqual(rc, 0)
        self.assertTrue(
            any(w in text for w in ("rollback", "roll back", "orphan", "newer than")),
            "deployed harness-core 0.2.3 > source 0.2.2 must be reported as a rollback/orphan "
            "case, not as 'rollout-plugins.sh not run'.\n" + out + err,
        )


if __name__ == "__main__":
    unittest.main()
