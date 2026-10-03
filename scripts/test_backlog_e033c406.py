#!/usr/bin/env python3
"""RED repro for backlog e033c406 (user-approved "close the hack route" gate).

After a PreToolUse guard DENIES a mutation of a main-tree path, the same session
can reach the very same target with a different spelling (an interpreter
wrapper) and nothing notices: guard-maintree-bash.py keeps no record of its own
denials, and its known holes list `python3 -c "open(p,'w')"` / `sh -c`.

The test drives the real guard in a throwaway repo + linked worktree (same setup
as test_maintree_isolation_guards.py):

  1. `rm <main>/tracked.rs`                                   -> exit 2 (control)
  2. same session: `python3 -c "import os; os.remove('<main>/tracked.rs')"`
     -> must NOT be exit 0 (ask/deny quoting the earlier refusal).

It also pins the small second gate the ticket asks for: rewiring
`core.hooksPath` from the main tree must not be exit 0.

Both are fixed: the interpreter retry since ae4543d5 (judged by effect), the
core.hooksPath rewiring since e033c406. The deny-ledger behaviour itself is
pinned in test_deny_ledger_e033c406.py.

    python3 scripts/test_backlog_e033c406.py
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
GUARD = os.path.join(SCRIPTS, "guard-maintree-bash.py")
SESSION = "e033c406-session"


class RetryAfterDenyIsDetected(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="e033c406.")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = os.path.join(self.tmp, "home")
        os.makedirs(self.home)
        self.main = os.path.join(self.tmp, "main")
        os.makedirs(self.main)
        subprocess.run(["git", "init", "-q", self.main], check=True)
        for k, v in (("user.email", "t@t"), ("user.name", "t")):
            subprocess.run(["git", "config", k, v], cwd=self.main, check=True)
        with open(os.path.join(self.main, "tracked.rs"), "w") as f:
            f.write("hi\n")
        subprocess.run(["git", "add", "-A"], cwd=self.main, check=True)
        subprocess.run(["git", "commit", "-qm", "init"], cwd=self.main, check=True)
        subprocess.run(
            ["git", "worktree", "add", "-q", os.path.join(self.tmp, "wtA"),
             "-b", "feat", "HEAD"],
            cwd=self.main, check=True,
        )

    def _guard(self, cmd: str) -> subprocess.CompletedProcess:
        env = dict(os.environ)
        env["CLAUDE_PROJECT_DIR"] = self.main
        env["HOME"] = self.home  # any per-session deny ledger lands here
        payload = {
            "session_id": SESSION,
            "hook_event_name": "PreToolUse",
            "cwd": self.main,
            "tool_name": "Bash",
            "tool_input": {"command": cmd},
        }
        return subprocess.run(
            [sys.executable, GUARD], cwd=self.main, input=json.dumps(payload),
            capture_output=True, text=True, env=env,
        )

    # Passes since ae4543d5: the guard now judges `python3 -c` payloads by
    # effect, so the retry is refused on its own (no deny ledger involved).
    def test_same_target_retry_via_interpreter_after_deny_is_not_allowed(self):
        target = os.path.join(self.main, "tracked.rs")
        first = self._guard("rm " + target)
        self.assertEqual(first.returncode, 2, "control: the plain rm must be denied")
        retry = self._guard(
            "python3 -c \"import os; os.remove('%s')\"" % target
        )
        self.assertNotEqual(
            retry.returncode, 0,
            "the denied target was re-reached via python3 -c in the same session "
            "and allowed (rc=0); stderr=%r" % retry.stderr,
        )

    # Passes since e033c406: guard-maintree-bash.py refuses rewiring the hook
    # directory (git config / -c / GIT_CONFIG_*), from any tree.
    def test_rewiring_core_hookspath_from_main_is_not_allowed(self):
        r = self._guard("git config core.hooksPath /dev/null")
        self.assertNotEqual(
            r.returncode, 0,
            "disabling the repo's hooks via core.hooksPath was allowed (rc=0)",
        )


if __name__ == "__main__":
    unittest.main()
