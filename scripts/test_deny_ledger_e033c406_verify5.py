"""Verifier round 5 for backlog e033c406 (on 7e7cc650). Written by the condukt
verifier, not the implementer.

Round 5 matches a git glob pathspec against the .githooks tree of the source
revision the command names (checkout REV -- ..., restore -s REV). Three
holes in that claimed class, each observed by the verifier (the first two with
real git, which wrote .githooks/zz-oldhook, a file present only in older
revisions):

  * `git checkout - -- "*oldhook"`: git reads `-` as @{-1}; the guard skips
    `-` as an option, so no revision is listed.
  * `git checkout ":/c1" -- "*oldhook"` and `git restore -s ":/c1" "*oldhook"`:
    a :/<message> revision. The guard lists `:/c1^{tree}`, which git reads as
    a message search for "c1^{tree}"; ls-tree fails, and
    `rev-parse --verify -q :/c1^{tree}` exits 1 silently, so the revision is
    SKIPPED as "nonexistent" while git itself resolves `:/c1` and writes.
  * a failing `git ls-tree` for restore's implicit HEAD source is skipped
    instead of refused (fault injected with a PATH wrapper), contrary to the
    docstring ("a git listing that fails or exits non-zero ... is refused").

Also pins (passing) the round-4 dir-glob over-block fix.

Hooks run as subprocesses; HOME and TMPDIR point at a temp dir.

    python3 -m unittest scripts/test_deny_ledger_e033c406_verify5.py
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
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="e033c406v5-"))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.main = os.path.join(self.tmp, "main")
        self.home = os.path.join(self.tmp, "home")
        self.tmpd = os.path.join(self.tmp, "t")
        for d in (os.path.join(self.main, "src"), os.path.join(self.main, "crates", "a"),
                  os.path.join(self.main, ".githooks"), self.home, self.tmpd):
            os.makedirs(d)
        for rel, body in ((".githooks/pre-commit", "GATE\n"), (".githooks/zz-oldhook", "OLD\n"),
                          ("src/a.rs", "a\n"), ("crates/a/Cargo.toml", "x\n")):
            with open(os.path.join(self.main, rel), "w") as f:
                f.write(body)
        _git(self.main, "init", "-q", "-b", "main")
        _git(self.main, "add", "-A")
        _git(self.main, "commit", "-qm", "c1")
        _git(self.main, "branch", "oldb")
        os.remove(os.path.join(self.main, ".githooks", "zz-oldhook"))
        _git(self.main, "add", "-A")
        _git(self.main, "commit", "-qm", "c2")
        _git(self.main, "checkout", "-q", "oldb")
        _git(self.main, "checkout", "-q", "main")  # @{-1} is oldb

    def bash(self, cmd, path_prefix=None):
        env = {k: v for k, v in os.environ.items()
               if k not in ("CLAUDECODE", "CLAUDE_CODE_ENTRYPOINT")}
        env.update(HOME=self.home, TMPDIR=self.tmpd, CLAUDE_PROJECT_DIR=self.main)
        if path_prefix:
            env["PATH"] = path_prefix + os.pathsep + env.get("PATH", "")
        payload = {"session_id": "v5", "tool_name": "Bash",
                   "tool_input": {"command": cmd}, "cwd": self.main}
        return subprocess.run([sys.executable, BASH], input=json.dumps(payload),
                              text=True, capture_output=True, env=env,
                              cwd=self.main, timeout=60)


class SourceRevisionListing(_Fixture):
    def test_named_revisions_refused(self):
        # closed in 7e7cc650: regression guard
        for cmd in ("git checkout HEAD~1 -- " + q("*oldhook"),
                    "git restore -s HEAD~1 " + q("*oldhook"),
                    "git checkout oldb -- " + q("*oldhook"),
                    "git checkout @{-1} -- " + q("*oldhook")):
            with self.subTest(cmd=cmd):
                self.assertEqual(self.bash(cmd).returncode, 2)

    def test_checkout_dash_previous_branch_is_refused(self):
        r = self.bash("git checkout - -- " + q("*oldhook"))
        self.assertEqual(r.returncode, 2, r.stderr)

    def test_message_search_revision_is_refused(self):
        for cmd in ("git checkout " + q(":/c1") + " -- " + q("*oldhook"),
                    "git restore -s " + q(":/c1") + " " + q("*oldhook")):
            with self.subTest(cmd=cmd):
                r = self.bash(cmd)
                self.assertEqual(r.returncode, 2, r.stderr)

    def test_failing_ls_tree_for_restore_head_is_refused(self):
        real = shutil.which("git")
        fake = os.path.join(self.tmp, "fakegit")
        os.makedirs(fake)
        with open(os.path.join(fake, "git"), "w") as f:
            f.write('#!/bin/bash\nfor a in "$@"; do [ "$a" = ls-tree ] && exit 1; done\n'
                    f'exec {real} "$@"\n')
        os.chmod(os.path.join(fake, "git"), 0o755)
        r = self.bash("git restore " + q("src/*"), path_prefix=fake)
        self.assertEqual(r.returncode, 2, r.stderr)


class DirGlobsAllowed(_Fixture):
    def test_ordinary_dir_globs_allowed(self):
        for cmd in ("git restore " + q("src/*"), "git checkout -- " + q("crates/*/Cargo.toml"),
                    "git restore -s HEAD~1 " + q("src/*"), "git rm --cached " + q("*.log")):
            with self.subTest(cmd=cmd):
                self.assertEqual(self.bash(cmd).returncode, 0)


if __name__ == "__main__":
    unittest.main()
