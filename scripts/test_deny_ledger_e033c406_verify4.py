"""Verifier round 4 for backlog e033c406 (on 2b5ce63b). Written by the condukt
verifier, not the implementer.

1. Pins (passing) the neighbours of the round-4 ./ and ../ normalisation of git
   glob pathspecs, so a regression is visible.

2. Pins an OVER-BLOCK against c0d4701c on ordinary git commands (convergence
   rule d). Since 55769b61, a git glob pathspec whose directory part is an
   ordinary directory is refused, because each glob COMPONENT is tested with
   the shape rule (_hook_protected with dotglob=True), and a bare `*`
   component "may be" a `.githooks` directory at any depth:

     git restore "src/*"              -> exit 2 (c0d4701c: 0)
     git checkout -- "src/*"          -> exit 2
     git rm -r "docs/*"               -> exit 2
     git stash push -- "src/*"        -> exit 2
     git restore "crates/*/src/main.rs" -> exit 2

   None of these can reach a hook file in a repo whose only hook dirs are
   <toplevel>/.githooks and <common-dir>/hooks, which round 3 already lists
   and matches against. Observed by the verifier on 55769b61, cf3a0e5c and
   2b5ce63b (worktree session); the verifier missed it in rounds 2 and 3
   because the corpus had no dir/* patterns.

Hooks run as subprocesses; HOME and TMPDIR point at a temp dir.

    python3 -m unittest scripts/test_deny_ledger_e033c406_verify4.py
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
BASH = os.path.join(HERE, "guard-maintree-bash.py")
Q = chr(39)


def q(s):
    return Q + s + Q


def _git(cwd, *args):
    subprocess.run(("git", "-c", "user.email=v@v", "-c", "user.name=v") + args,
                   cwd=cwd, check=True, capture_output=True)


class _Fixture(unittest.TestCase):
    def setUp(self):
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="e033c406v4-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.main = os.path.join(self.tmp, "main")
        self.wt = os.path.join(self.tmp, "wt")
        self.home = os.path.join(self.tmp, "home")
        self.tmpd = os.path.join(self.tmp, "t")
        for d in (os.path.join(self.main, "src"), os.path.join(self.main, "docs"),
                  os.path.join(self.main, "crates", "a", "src"),
                  os.path.join(self.main, ".githooks"), self.home, self.tmpd):
            os.makedirs(d)
        for rel in ("src/a.rs", "docs/x.md", "crates/a/src/main.rs"):
            with open(os.path.join(self.main, rel), "w") as f:
                f.write("x\n")
        with open(os.path.join(self.main, ".githooks", "pre-commit"), "w") as f:
            f.write("#!/bin/sh\n")
        _git(self.main, "init", "-q")
        _git(self.main, "add", "-A")
        _git(self.main, "commit", "-qm", "init")
        _git(self.main, "worktree", "add", "-q", self.wt, "-b", "wt")

    def bash(self, cmd, cwd):
        env = {k: v for k, v in os.environ.items()
               if k not in ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT")}
        env.update(HOME=self.home, TMPDIR=self.tmpd, CLAUDE_PROJECT_DIR=cwd)
        payload = {"session_id": "v4", "tool_name": "Bash",
                   "tool_input": {"command": cmd}, "cwd": cwd}
        return subprocess.run([sys.executable, BASH], input=json.dumps(payload),
                              text=True, capture_output=True, env=env,
                              cwd=self.main, timeout=60)


class NormalisationNeighbours(_Fixture):
    REFUSED = [
        "git checkout HEAD~1 -- " + q("./*pre-commit"),
        "git checkout HEAD~1 -- " + q(".//*pre-commit"),
        "git checkout HEAD~1 -- " + q("src/./../*pre-commit"),
        "git checkout HEAD~1 -- " + q(":(top)./*pre-commit"),
        "git checkout HEAD~1 -- " + q(":/./*pre-commit"),
        "git checkout HEAD~1 -- " + q("./.githooks/../.githooks/*"),
        "git checkout HEAD~1 -- " + q(":(icase)./*PRE-COMMIT"),
        "git checkout HEAD~1 -- " + q(":(glob)./**/pre-commit"),
        "cd src && git checkout HEAD~1 -- " + q("../*pre-commit"),
        "cd src && git checkout HEAD~1 -- " + q("./../*pre-commit"),
        "git -C src checkout HEAD~1 -- " + q("../*pre-commit"),
    ]
    ALLOWED = [
        "git checkout -- " + q("./*.rs"),
        "cd src && git checkout -- " + q("../*.rs"),
        "git checkout -- ./src/a.rs",
        "git add ./src/a.rs",
        "git rm " + q("./*.orig"),
    ]

    def test_refused(self):
        for cwd in (self.main, self.wt):
            for cmd in self.REFUSED:
                with self.subTest(cwd=cwd, cmd=cmd):
                    self.assertEqual(self.bash(cmd, cwd).returncode, 2)

    def test_allowed(self):
        for cmd in self.ALLOWED:
            with self.subTest(cmd=cmd):
                self.assertEqual(self.bash(cmd, self.wt).returncode, 0)


class DirGlobOverBlock(_Fixture):
    """c0d4701c allowed every one of these; none can reach a hook file."""
    CMDS = [
        "git restore " + q("src/*"),
        "git checkout -- " + q("src/*"),
        "git rm -r " + q("docs/*"),
        "git stash push -- " + q("src/*"),
        "git restore " + q("crates/*/src/main.rs"),
    ]

    def test_dir_glob_pathspecs_are_allowed_in_a_worktree(self):
        for cmd in self.CMDS:
            with self.subTest(cmd=cmd):
                r = self.bash(cmd, self.wt)
                self.assertEqual(r.returncode, 0, r.stderr)


if __name__ == "__main__":
    unittest.main()
