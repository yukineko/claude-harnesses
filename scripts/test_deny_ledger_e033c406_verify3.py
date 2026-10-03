"""Verifier round 3 for backlog e033c406 (on cf3a0e5c). Written by the condukt
verifier, not the implementer.

Pins the ledger self-protection on the Edit/Write side: an edit into the deny
ledger directory is refused from any tree.

User ruling 2026-10-04: the Bash side of e033c406 is dropped with the observing
guard-maintree-bash.py, so the Bash ledger-dir and dot-relative glob pathspec
tests that lived here were removed.

Hooks run as subprocesses; HOME and TMPDIR point at a temp dir.

    python3 -m unittest scripts/test_deny_ledger_e033c406_verify3.py
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
EDIT = os.path.join(HERE, "guard-maintree-edit.py")


def _git(cwd, *args):
    subprocess.run(("git", "-c", "user.email=v@v", "-c", "user.name=v") + args,
                   cwd=cwd, check=True, capture_output=True)


class _Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="e033c406v3-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.main = os.path.join(self.tmp, "main")
        self.wt = os.path.join(self.tmp, "wt")
        self.home = os.path.join(self.tmp, "home")
        self.tmpd = os.path.join(self.tmp, "t")
        for d in (os.path.join(self.main, "src"), os.path.join(self.main, ".githooks"),
                  os.path.join(self.home, ".claude", "state", "maintree-deny"), self.tmpd):
            os.makedirs(d)
        with open(os.path.join(self.main, "src", "a"), "w") as f:
            f.write("a\n")
        with open(os.path.join(self.main, ".githooks", "pre-commit"), "w") as f:
            f.write("#!/bin/sh\n")
        _git(self.main, "init", "-q")
        _git(self.main, "add", "-A")
        _git(self.main, "commit", "-qm", "init")
        _git(self.main, "worktree", "add", "-q", self.wt, "-b", "wt")
        self.ledger = os.path.join(self.home, ".claude", "state", "maintree-deny")

    def run_hook(self, script, payload, proj):
        env = {k: v for k, v in os.environ.items()
               if k not in ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT")}
        env.update(HOME=self.home, TMPDIR=self.tmpd, CLAUDE_PROJECT_DIR=proj)
        return subprocess.run([sys.executable, script], input=json.dumps(payload),
                              text=True, capture_output=True, env=env,
                              cwd=self.main, timeout=60)


class LedgerSelfProtection(_Fixture):
    def test_edit_tool_into_ledger_dir_refused(self):
        r = self.run_hook(EDIT, {"session_id": "v3", "tool_name": "Write",
                                 "tool_input": {"file_path": os.path.join(self.ledger, "x.jsonl"),
                                                "content": ""}}, proj=self.wt)
        self.assertEqual(r.returncode, 2, r.stderr)


if __name__ == "__main__":
    unittest.main()
