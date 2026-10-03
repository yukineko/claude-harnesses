#!/usr/bin/env python3
"""Repro tests for backlog 73c2c089.

(a) scripts/check-plugin-rollout.py treats an ABSENT plugin registry / settings
    file as "SKIP ... not a failure" and exits 0, while an UNPARSEABLE one exits
    non-zero. Both paths are selectable by environment variable
    (CLAUDE_PLUGIN_REGISTRY / CLAUDE_SETTINGS), so pointing them at a missing path
    turns the "is the GATE plugin enabled" dimension green without checking it.
    Cannot-determine must resolve to the restrictive side (CLAUDE.md 3).

(b) .githooks/pre-push demotes rc=2 (a GATE crate installed but not enabled = an
    inert gate) to "advisory only — the push is not blocked."

Both open; marked expectedFailure. The control tests (unparseable registry exits
non-zero) are plain tests and must stay green.
"""

import os
import re
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CHECK = REPO / "scripts" / "check-plugin-rollout.py"
PRE_PUSH = REPO / ".githooks" / "pre-push"


def run_check(registry, settings):
    with tempfile.TemporaryDirectory(prefix="bl-73c2c089-") as home:
        env = dict(os.environ)
        env.update(
            HOME=home,
            CLAUDE_PLUGIN_REGISTRY=str(registry),
            CLAUDE_SETTINGS=str(settings),
        )
        return subprocess.run(
            [sys.executable, str(CHECK)],
            cwd=REPO,
            env=env,
            capture_output=True,
            text=True,
            timeout=300,
        )


class AbsentIsNotAPass(unittest.TestCase):
    def test_control_unparseable_registry_is_nonzero(self):
        with tempfile.TemporaryDirectory(prefix="bl-73c2c089-") as d:
            reg = Path(d) / "installed_plugins.json"
            reg.write_text("{not json")
            p = run_check(reg, Path(d) / "absent-settings.json")
            self.assertNotEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_absent_registry_and_settings_is_not_exit_zero(self):
        with tempfile.TemporaryDirectory(prefix="bl-73c2c089-") as d:
            p = run_check(Path(d) / "no-such-registry.json", Path(d) / "no-such-settings.json")
            self.assertNotEqual(
                p.returncode,
                0,
                "absent registry/settings could not be checked against, so the "
                "verdict is undetermined and must not be exit 0. output:\n"
                + p.stdout
                + p.stderr,
            )


class PrePushDoesNotDemoteInertGate(unittest.TestCase):
    @unittest.expectedFailure
    def test_rc2_branch_blocks_the_push(self):
        text = PRE_PUSH.read_text()
        m = re.search(r"\n\s*2\)\n(.*?)\n\s*;;", text, re.S)
        self.assertIsNotNone(m, "precondition: pre-push has an rc=2 case branch")
        body = m.group(1)
        self.assertNotIn(
            "the push is not blocked",
            body,
            "rc=2 = a GATE crate is installed but not enabled (inert); pre-push "
            "must not demote it to advisory. branch body:\n" + body,
        )
        self.assertRegex(body, r"\bexit\s+[1-9]", "rc=2 branch must exit non-zero")


if __name__ == "__main__":
    unittest.main()
