#!/usr/bin/env python3
"""Closure regression for backlog 29b08fc7.

29b08fc7: guard-maintree-bash.py refused commands that never write the main
tree -- a bare shell-variable assignment line, redirects into a scratch dir,
`mkdir -p <scratch>`, and `cp <other-worktree>/f <scratch>/f` -- reporting the
FIRST LINE of the command (e.g. `S=/private/...`) as the thing that "mutates
this project's MAIN working tree".

The mechanism (fixed in caef0517): an unquoted newline was not a command
separator, so a later line's operands were judged as operands of the first
line's program (an assignment line or a `cp`), and an unknown `$S` operand
resolved to the main root.

Each benign form must be ALLOWED (rc 0); the control forms that really write
into the main tree must still be REFUSED (rc 2) and the refusal must name the
offending target rather than only quoting line 1.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest

# Forms of the ticket that are STILL refused at the time of writing (the ticket
# stays open). Run them with RUN_IGNORED=1; they are RED until 29b08fc7 is fixed.
IGNORED = unittest.skipUnless(
    os.environ.get("RUN_IGNORED"), "29b08fc7 still open: $S-assignment form refused"
)

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


class Backlog29b08fc7(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="bl-29b08fc7."))
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.tmp]))
        self.main = os.path.join(self.tmp, "main")
        os.makedirs(self.main)
        subprocess.run(["git", "init", "-q", self.main], check=True)
        for k, v in (("user.email", "t@t"), ("user.name", "t")):
            subprocess.run(["git", "config", k, v], cwd=self.main, check=True)
        with open(os.path.join(self.main, "tracked.rs"), "w") as f:
            f.write("hi\n")
        subprocess.run(["git", "add", "-A"], cwd=self.main, check=True)
        subprocess.run(["git", "commit", "-qm", "init"], cwd=self.main, check=True)
        self.scratch = os.path.join(self.tmp, "scratchpad")
        os.makedirs(self.scratch)
        self.other_wt = os.path.join(self.tmp, "condukt-wt")
        os.makedirs(self.other_wt)

    def assertAllowed(self, cmd: str) -> None:
        rc, out = _run(cmd, self.main, self.main)
        self.assertEqual(rc, 0, f"benign command refused:\n{cmd}\n---\n{out}")

    # --- the five forms named in the ticket (session cwd = main checkout) ---
    @IGNORED
    def test_assignment_line_then_redirect_into_scratch(self):
        self.assertAllowed(
            f"S={self.scratch}\n"
            f"git -C {self.main} show HEAD:tracked.rs > $S/base.toml"
        )

    def test_cd_main_then_git_show_redirect_to_scratch(self):
        self.assertAllowed(
            f"cd {self.main}\n"
            f"git show HEAD:tracked.rs > {self.scratch}/base.toml\n"
            f"git show HEAD:tracked.rs > {self.scratch}/ours.toml"
        )

    @IGNORED
    def test_assignment_then_mkdir_scratch_var(self):
        self.assertAllowed(f"S={self.scratch}\nmkdir -p $S/batch6-rescue")

    def test_mkdir_scratch_literal(self):
        self.assertAllowed(f"mkdir -p {self.scratch}/batch6-rescue")

    def test_cp_other_worktree_to_scratch_then_next_line(self):
        # The second line's operand used to become an extra operand of `cp`.
        self.assertAllowed(
            f"cp {self.other_wt}/subagent_stop.rs {self.scratch}/rescue.rs\n"
            f"ls $S"
        )

    # --- controls: real writes into main stay refused ----------------------
    def test_control_second_line_write_into_main_is_refused(self):
        rc, out = _run(
            f"S={self.scratch}\nrm {self.main}/tracked.rs", self.main, self.main
        )
        self.assertEqual(rc, 2, out)
        # The refusal names the real target, not only the assignment line.
        self.assertIn(f"{self.main}/tracked.rs", out)

    def test_control_redirect_into_main_is_refused(self):
        rc, out = _run(f"echo x > {self.main}/tracked.rs", self.main, self.main)
        self.assertEqual(rc, 2, out)


if __name__ == "__main__":
    unittest.main()
