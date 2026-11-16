#!/usr/bin/env python3
"""RED: check-unlisted-gates does not count the SessionStart invocation chain.

Since 3858c833f the plugin-rollout check runs at SessionStart only:

  .claude/settings.json  SessionStart hook
    -> python3 "$CLAUDE_PROJECT_DIR/scripts/session-worktree-init.py"
    -> scripts/session-rollout-drift.py   (subprocess, _report_rollout_drift)
    -> scripts/check-plugin-rollout.py    (subprocess, CHECKER)

check-unlisted-gates.py only reads git hook bodies, so it reports
check-plugin-rollout.py as "NOT invoked by any active hook" although a hook
(the Claude Code SessionStart hook) really runs it. Expected: a scanner that
an active hook reaches through a real invocation chain counts as invoked —
established by tracing the chain, NOT by an allowlist (the scanner's own
docstring forbids an allowlist).

Control: a check-*.py that nothing invokes must still be reported unlisted,
so the fix cannot pass by blanket-listing everything.

Both tests run the real scanner against a throwaway git repo that is a COPY of
this checkout's .githooks/, .claude/settings.json and scripts/ (so the real
chain is exercised, no fix shape is assumed), plus one dummy uncalled scanner.
Nothing here writes to the real repository.
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
DUMMY = "check-zz-dummy-never-invoked.py"


class SessionStartChain(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root = Path(tempfile.mkdtemp(prefix="unlisted-sessionstart-")).resolve()
        cls.repo = cls.root / "repo"
        cls.repo.mkdir()
        cls.env = dict(
            PATH="/usr/bin:/bin:/usr/sbin:/sbin",
            HOME=str(cls.root),
            LC_ALL="C",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CEILING_DIRECTORIES=str(cls.root),
        )
        subprocess.run(["git", "init", "-q", "-b", "main"], cwd=cls.repo,
                       env=cls.env, check=True, capture_output=True)
        shutil.copytree(SRC / ".githooks", cls.repo / ".githooks", symlinks=True)
        (cls.repo / ".claude").mkdir()
        shutil.copy2(SRC / ".claude" / "settings.json", cls.repo / ".claude" / "settings.json")
        shutil.copytree(SRC / "scripts", cls.repo / "scripts", symlinks=True,
                        ignore=shutil.ignore_patterns("__pycache__", "*.pyc"))
        (cls.repo / "scripts" / DUMMY).write_text(
            "#!/usr/bin/env python3\nraise SystemExit(0)\n")
        p = subprocess.run(
            [sys.executable, str(SCANNER), "--hooks-dir", str(cls.repo / ".githooks")],
            cwd=cls.repo, env=cls.env, capture_output=True, text=True, timeout=120,
        )
        cls.rc, cls.out, cls.err = p.returncode, p.stdout, p.stderr
        cls.unlisted = {
            line.strip()[len("scripts/"):]
            for line in p.stderr.splitlines()
            if line.startswith("  scripts/")
        }

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, True)

    def _detail(self):
        return "rc=%d\nstdout:\n%s\nstderr:\n%s" % (self.rc, self.out, self.err)

    def test_precondition_settings_sessionstart_reaches_rollout_checker(self):
        # Guard: the chain this test is about really exists in the copied tree.
        settings = (self.repo / ".claude" / "settings.json").read_text()
        self.assertIn("scripts/session-worktree-init.py", settings)
        init = (self.repo / "scripts" / "session-worktree-init.py").read_text()
        self.assertIn('"session-rollout-drift.py"', init)
        drift = (self.repo / "scripts" / "session-rollout-drift.py").read_text()
        self.assertIn('CHECKER = "scripts/check-plugin-rollout.py"', drift)

    def test_scanner_produced_a_verdict(self):
        # 0 or 1 is a verdict; 2 (undetermined) would make the other asserts vacuous.
        self.assertIn(self.rc, (0, 1), self._detail())

    def test_rollout_checker_reached_via_sessionstart_is_not_unlisted(self):
        # RED at f6a19c390: check-plugin-rollout.py is listed as unlisted.
        self.assertNotIn("check-plugin-rollout.py", self.unlisted, self._detail())

    def test_control_uncalled_dummy_scanner_is_still_unlisted(self):
        self.assertEqual(self.rc, 1, self._detail())
        self.assertIn(DUMMY, self.unlisted, self._detail())


if __name__ == "__main__":
    unittest.main()
