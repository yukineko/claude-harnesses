"""Verifier round 3 for backlog e033c406 (on cf3a0e5c). Written by the condukt
verifier, not the implementer.

Pins a bypass in a class the guard-maintree-bash.py docstring claims: a git glob
pathspec is matched against every entry that exists under
<toplevel>/.githooks, repo-relative and prefixed with the cwd path in the repo
unless :(top) or :/. git normalises ./ and ../ in a pathspec before matching;
the guard does not, so

  * git checkout HEAD~1 -- "./*pre-commit"            (from the top level)
  * cd src && git checkout HEAD~1 -- "../*pre-commit" (from a subdirectory)

are allowed, and real git restores the old .githooks/pre-commit for both
(observed by the verifier).

Also pins (passing) the round-3 ledger self-protection forms the verifier
probed, so a regression is visible.

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
BASH = os.path.join(HERE, "guard-maintree-bash.py")
EDIT = os.path.join(HERE, "guard-maintree-edit.py")
Q = chr(39)


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

    def bash(self, cmd, cwd):
        return self.run_hook(BASH, {"session_id": "v3", "tool_name": "Bash",
                                    "tool_input": {"command": cmd}, "cwd": cwd}, proj=cwd)

    def refused_everywhere(self, cmd):
        for cwd in (self.main, self.wt):
            with self.subTest(cwd=cwd, cmd=cmd):
                r = self.bash(cmd, cwd)
                self.assertEqual(r.returncode, 2, r.stderr)


class DotRelativeGlobPathspec(_Fixture):
    def test_dot_slash_glob_matching_hook_file_is_refused(self):
        self.refused_everywhere("git checkout HEAD~1 -- " + Q + "./*pre-commit" + Q)

    def test_dotdot_glob_from_subdir_matching_hook_file_is_refused(self):
        self.refused_everywhere("cd src && git checkout HEAD~1 -- " + Q + "../*pre-commit" + Q)

    def test_plain_and_top_globs_refused(self):
        # closed in e2399b08: regression guard
        self.refused_everywhere("git checkout HEAD~1 -- " + Q + "*pre-commit" + Q)
        self.refused_everywhere("cd src && git checkout HEAD~1 -- " + Q + ":/*pre-commit" + Q)


class LedgerSelfProtection(_Fixture):
    def test_writes_into_ledger_dir_refused(self):
        L = self.ledger
        for cmd in (f"rm {L}/x.jsonl", "rm ~/.claude/state/maintree-deny/*.jsonl",
                    "rm -rf ~/.claude", "find ~/.claude -name " + Q + "*.jsonl" + Q + " -delete",
                    f"python3 -c \"import os; os.remove({Q}{L}/x.jsonl{Q})\"",
                    f"ln -sf /dev/null {L}/x.jsonl", f"touch -t 202001010000 {L}/x.jsonl",
                    "cd ~ && rm -rf .claude/state/maintree-deny", "rm -rf $TMPDIR/*"):
            self.refused_everywhere(cmd)

    def test_reads_and_neighbours_allowed(self):
        L = self.ledger
        for cmd in (f"cat {L}/x.jsonl", f"ls {L}", "rm -f $TMPDIR/foo.log",
                    "touch ~/.claude/settings.local.json"):
            with self.subTest(cmd=cmd):
                self.assertEqual(self.bash(cmd, self.wt).returncode, 0)

    def test_edit_tool_into_ledger_dir_refused(self):
        r = self.run_hook(EDIT, {"session_id": "v3", "tool_name": "Write",
                                 "tool_input": {"file_path": os.path.join(self.ledger, "x.jsonl"),
                                                "content": ""}}, proj=self.wt)
        self.assertEqual(r.returncode, 2, r.stderr)


if __name__ == "__main__":
    unittest.main()
