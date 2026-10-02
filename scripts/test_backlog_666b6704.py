#!/usr/bin/env python3
"""Repro test for backlog 666b6704: with core.hooksPath set to the MAIN tree's
absolute .githooks (as on this machine: `git config --get core.hooksPath` ->
/Users/yuki/src/harness/.githooks), a gate added to .githooks/pre-commit inside a
worktree is never run by commits in that worktree until it is merged. The hook
LIST comes from main while the scanner BODIES come from the worktree
(`REPO="$(git rev-parse --show-toplevel)"`, `path="$REPO/scripts/$scanner"`):
split-brain, and the new gate is dark, not red.

Fixture: a throwaway repo carrying the REAL .githooks/* from this checkout, every
scanner they name replaced by a stub that logs its own name and exits 0, the real
scripts/gate-bypass.py, and core.hooksPath set to the main tree's ABSOLUTE
.githooks. A worktree adds `run check-brand-new.py brand-new` to its own
pre-commit plus the stub, then commits. Not dark means: the new gate ran, OR the
commit was refused because the hook it ran is not the worktree's own.

Control: the same flow with the RELATIVE hooksPath (`.githooks`, the form CLAUDE.md
documents) does run the new gate — proving the fixture can observe GREEN.
Open defect; expectedFailure on the absolute case.
"""

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
HOOKS = ("pre-commit", "post-commit", "pre-merge-commit", "post-merge", "pre-push", "commit-msg")
STUB = """#!/usr/bin/env python3
import os
log = os.environ.get("GATE_TEST_LOG")
if log:
    with open(log, "a") as fh:
        fh.write(%r + "\\n")
"""


def scanners():
    names = set()
    for h in HOOKS:
        names.update(re.findall(r"\b(check-[\w-]+\.py)\b", (SRC / ".githooks" / h).read_text()))
    return sorted(names)


class Fx:
    def __init__(self, test, hooks_path_mode):
        self.root = Path(tempfile.mkdtemp(prefix="bl-666b6704-")).resolve()
        test.addCleanup(shutil.rmtree, self.root, True)
        self.log = self.root / "ran.log"
        self.log.write_text("")
        stub_bin = self.root / "stub-bin"
        stub_bin.mkdir()
        (stub_bin / "cargo").write_text("#!/bin/sh\nexit 0\n")
        (stub_bin / "cargo").chmod(0o755)
        condukt = stub_bin / "condukt-stub"
        condukt.write_text("#!/bin/sh\nexit 0\n")
        condukt.chmod(0o755)
        self.env = dict(
            PATH=str(stub_bin) + os.pathsep + "/usr/bin:/bin:/usr/sbin:/sbin",
            HOME=str(self.root),
            GATE_TEST_LOG=str(self.log),
            CONDUKT_BIN=str(condukt),
            LC_ALL="C",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CEILING_DIRECTORIES=str(self.root.parent),
            GIT_AUTHOR_NAME="t",
            GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t",
            GIT_COMMITTER_EMAIL="t@example.invalid",
        )
        self.main = self.root / "main"
        self.main.mkdir()
        self.git(self.main, "init", "-q", "-b", "main")
        (self.main / ".githooks").mkdir()
        for h in HOOKS:
            shutil.copy2(SRC / ".githooks" / h, self.main / ".githooks" / h)
            (self.main / ".githooks" / h).chmod(0o755)
        (self.main / "scripts").mkdir()
        shutil.copy2(SRC / "scripts" / "gate-bypass.py", self.main / "scripts" / "gate-bypass.py")
        for s in scanners():
            (self.main / "scripts" / s).write_text(STUB % s)
        hp = str(self.main / ".githooks") if hooks_path_mode == "absolute" else ".githooks"
        self.git(self.main, "config", "core.hooksPath", hp)
        self.git(self.main, "config", "commit.gpgsign", "false")
        (self.main / "seed.txt").write_text("seed\n")
        self.git(self.main, "add", "-A")
        self.git(self.main, "commit", "-q", "-m", "seed")

    def git(self, cwd, *args, check=True):
        p = subprocess.run(["git", *args], cwd=cwd, env=self.env, capture_output=True, text=True)
        if check and p.returncode != 0:
            raise AssertionError("git %s failed (%d)\n%s%s" % (args, p.returncode, p.stdout, p.stderr))
        return p

    def add_gate_in_worktree(self):
        wt = self.root / "wt"
        self.git(self.main, "worktree", "add", "-q", "-b", "feat", str(wt))
        pc = wt / ".githooks" / "pre-commit"
        text = pc.read_text()
        anchor = re.search(r"^run check-clippy-lints\.py[^\n]*\n", text, re.M)
        assert anchor, "precondition: pre-commit has the clippy-lints run line"
        text = text[: anchor.end()] + "run check-brand-new.py brand-new\n" + text[anchor.end():]
        pc.write_text(text)
        (wt / "scripts" / "check-brand-new.py").write_text(STUB % "check-brand-new.py")
        # Land the wiring in the worktree branch first (as the item did).
        self.git(wt, "add", "-A")
        self.git(wt, "commit", "-q", "-m", "wire brand-new gate")
        self.log.write_text("")
        (wt / "work.txt").write_text("work\n")
        self.git(wt, "add", "work.txt")
        p = self.git(wt, "commit", "-q", "-m", "work in worktree", check=False)
        ran = [l for l in self.log.read_text().splitlines() if l]
        return p, ran


class WorktreeGateIsNotDark(unittest.TestCase):
    def test_control_relative_hookspath_runs_worktree_gate(self):
        p, ran = Fx(self, "relative").add_gate_in_worktree()
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("check-brand-new.py", ran)

    @unittest.expectedFailure
    def test_absolute_hookspath_does_not_leave_worktree_gate_dark(self):
        p, ran = Fx(self, "absolute").add_gate_in_worktree()
        self.assertTrue(ran, "precondition: some gate ran at all (log: %r, stderr: %r)" % (ran, p.stderr))
        self.assertTrue(
            "check-brand-new.py" in ran or p.returncode != 0,
            "the worktree's new gate never ran and the commit went through (rc=%d): "
            "dark, not red. gates that ran: %r" % (p.returncode, ran),
        )


if __name__ == "__main__":
    unittest.main()
