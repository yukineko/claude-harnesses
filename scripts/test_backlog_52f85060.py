#!/usr/bin/env python3
"""Backlog 52f85060 — RED repro.

scripts/check-unlisted-gates.py resolves the hook dir ONLY from core.hooksPath
(or <git common dir>/hooks when unset). A scratch clone has core.hooksPath
unset, so `--repo <clone>` reads the clone's .git/hooks, finds no pre-commit,
and returns UNDETERMINED (2) — and there is no CLI option to point the check at
the clone's .githooks instead. Setting core.hooksPath in the clone is the only
route, and that is refused by guard-maintree-bash in this environment.

The control pins the observed rc 2; the expected failure asserts that the
check can be pointed at a hook dir explicitly (`--hooks-dir`) and reach a real
verdict (0 here: the fixture's one scanner is invoked).

Run: python3 -m pytest --runxfail scripts/test_backlog_52f85060.py
"""

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
SCANNER = SRC / "scripts" / "check-unlisted-gates.py"

PRE_COMMIT = """#!/bin/sh
set -u
REPO="$(git rev-parse --show-toplevel)"
rc=0
run() {
    scanner="$1"
    path="$REPO/scripts/$scanner"
    python3 "$path" || rc=1
}
run check-a.py
exit "$rc"
"""


class HooksDirOption(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="52f85060-")).resolve()
        self.addCleanup(shutil.rmtree, self.root, True)
        self.env = dict(
            PATH="/usr/bin:/bin:/usr/sbin:/sbin",
            HOME=str(self.root),
            LC_ALL="C",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CEILING_DIRECTORIES=str(self.root.parent),
        )
        self.repo = self.root / "clone"
        self.repo.mkdir()
        subprocess.run(
            ["git", "init", "-q"], cwd=self.repo, env=self.env, check=True
        )
        (self.repo / "scripts").mkdir()
        (self.repo / "scripts" / "check-a.py").write_text(
            "raise SystemExit(0)\n", encoding="utf-8"
        )
        hooks = self.repo / ".githooks"
        hooks.mkdir()
        pc = hooks / "pre-commit"
        pc.write_text(PRE_COMMIT, encoding="utf-8")
        pc.chmod(0o755)

    def _run(self, *args):
        return subprocess.run(
            [sys.executable, str(SCANNER), *args],
            cwd=self.root,
            env=self.env,
            capture_output=True,
            text=True,
            stdin=subprocess.DEVNULL,
        )

    def test_control_repo_without_hookspath_is_undetermined(self):
        p = self._run("--repo", str(self.repo))
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)

    def test_hooks_dir_option_checks_a_clone_without_hookspath(self):
        p = self._run(
            "--repo", str(self.repo), "--hooks-dir", str(self.repo / ".githooks")
        )
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)


if __name__ == "__main__":
    unittest.main()
