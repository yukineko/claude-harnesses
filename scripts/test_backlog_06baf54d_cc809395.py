#!/usr/bin/env python3
"""Shared-failure pin for backlog 06baf54d (closed as DUPLICATE) and cc809395.

Both tickets: guard-maintree-bash.py resolves RELATIVE paths against the main
tree root ($CLAUDE_PROJECT_DIR) even when the session's cwd is a linked
worktree, so an edit of the worktree's own file (`sed -i ... crates/x`) is
refused as a main-tree mutation.

Fixed by ae4543d5 (the guard now resolves relative paths against the hook
payload's `cwd`), so the former skips were removed. The control (a relative
write while cwd IS the main tree) must stay refused, so a fix cannot pass by
simply allowing every relative path.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
GUARD = os.path.join(SCRIPTS, "guard-maintree-bash.py")


def _run(cmd: str, cwd: str, main: str) -> tuple[int, str]:
    env = dict(os.environ, CLAUDE_PROJECT_DIR=main)
    p = subprocess.run(
        [sys.executable, GUARD],
        input=json.dumps({"tool_name": "Bash", "tool_input": {"command": cmd}}),
        capture_output=True, text=True, cwd=cwd, env=env,
    )
    return p.returncode, p.stderr + p.stdout


class Backlog06baf54dCc809395(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="bl-06baf54d."))
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.tmp]))
        self.main = os.path.join(self.tmp, "main")
        os.makedirs(os.path.join(self.main, "crates", "x"))
        subprocess.run(["git", "init", "-q", self.main], check=True)
        for k, v in (("user.email", "t@t"), ("user.name", "t")):
            subprocess.run(["git", "config", k, v], cwd=self.main, check=True)
        with open(os.path.join(self.main, "crates", "x", "Cargo.toml"), "w") as f:
            f.write('version = "0.1.0"\n')
        subprocess.run(["git", "add", "-A"], cwd=self.main, check=True)
        subprocess.run(["git", "commit", "-qm", "init"], cwd=self.main, check=True)
        self.wt = os.path.join(self.tmp, "wt")
        subprocess.run(
            ["git", "worktree", "add", "-q", self.wt, "-b", "feat", "HEAD"],
            cwd=self.main, check=True,
        )

    def test_relative_sed_in_worktree_cwd_is_allowed(self):
        rc, out = _run(
            "sed -i '' 's/0.1.0/0.1.1/' crates/x/Cargo.toml", self.wt, self.main
        )
        self.assertEqual(rc, 0, out)

    def test_relative_redirect_in_worktree_cwd_is_allowed(self):
        rc, out = _run("echo x > crates/x/new.txt", self.wt, self.main)
        self.assertEqual(rc, 0, out)

    def test_control_relative_sed_in_main_cwd_is_refused(self):
        rc, out = _run(
            "sed -i '' 's/0.1.0/0.1.1/' crates/x/Cargo.toml", self.main, self.main
        )
        self.assertEqual(rc, 2, out)


if __name__ == "__main__":
    unittest.main()
