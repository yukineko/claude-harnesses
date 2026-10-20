#!/usr/bin/env python3
"""Closure regression for backlog 4a66378d.

4a66378d: crates/donegate/tests/giveup_sentinel.rs had two tests failing with
"apparatus: under the cap donegate must block" (observed at 5a2efb46). Root
cause (165f6e07): the fixture set the session only in the hook payload, while
harness_core::repeat keys the second-occurrence waiver on the
CLAUDE_CODE_SESSION_ID env var, so the spawned donegate inherited whatever
session ran `cargo test` -- red inside a Claude Code session, green outside.

Property pinned: the giveup_sentinel suite is green REGARDLESS of the ambient
session environment -- both with an arbitrary inherited
CLAUDE_CODE_SESSION_ID and with it unset.
"""

from __future__ import annotations

import os
import subprocess
import unittest
import uuid

REPO = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))


def _run_suite(env: dict) -> subprocess.CompletedProcess:
    cargo = os.environ.get("CARGO", "cargo")
    return subprocess.run(
        [cargo, "test", "-p", "donegate", "--test", "giveup_sentinel"],
        cwd=REPO, env=env, capture_output=True, text=True,
    )


class Backlog4a66378d(unittest.TestCase):
    def test_green_with_an_ambient_session_id_inherited(self):
        env = dict(os.environ, CLAUDE_CODE_SESSION_ID=f"ambient-{uuid.uuid4()}")
        p = _run_suite(env)
        self.assertEqual(p.returncode, 0, p.stdout[-3000:] + p.stderr[-2000:])

    def test_green_with_no_session_id(self):
        env = {k: v for k, v in os.environ.items() if k != "CLAUDE_CODE_SESSION_ID"}
        p = _run_suite(env)
        self.assertEqual(p.returncode, 0, p.stdout[-3000:] + p.stderr[-2000:])


if __name__ == "__main__":
    unittest.main()
