#!/usr/bin/env python3
"""Closure regression for backlog 55826e4f (closed as DUPLICATE of f036e218).

55826e4f: guard-maintree-bash.py refused an `rm -rf` whose target was only under
the system temp dir. Re-measured (2026-10-02, at 874d6d47 = the ticket's rev AND
at HEAD): a LITERAL /tmp target is allowed; the only spellings that are refused
are the ones whose target holds an unexpanded shell variable (`$TMPDIR/...`,
`D=...; rm -rf $D`) — i.e. the f036e218 mechanism (variables are not expanded,
the empty literal prefix resolves against the main root).

* `test_literal_tmp_target_is_allowed` is GREEN: it pins the half that is fixed
  (never broken) so a regression on the literal form is visible.
* `test_variable_tmp_target_is_allowed` pins the SHARED open failure of both ids.
  It is skipped (RED today) until f036e218 is ruled/fixed; un-skip it then. If the
  human ruling is that an unexpanded variable must stay refused (fail-closed),
  delete this test together with closing f036e218 — do not flip its assertion.
"""
import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

GUARD = Path(__file__).resolve().parent / "guard-maintree-bash.py"


def run_guard(main_root: str, command: str) -> subprocess.CompletedProcess:
    payload = {
        "session_id": "s",
        "cwd": main_root,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    }
    env = dict(os.environ, CLAUDE_PROJECT_DIR=main_root)
    return subprocess.run(
        ["python3", str(GUARD)],
        input=json.dumps(payload),
        capture_output=True,
        text=True,
        cwd=main_root,
        env=env,
    )


class GuardTmpTargets(unittest.TestCase):
    def setUp(self):
        # <base>/main is the "main tree"; <base>/scratch/condukt-wt-scope-* is the
        # out-of-tree target. The two branches are disjoint (like /tmp vs the
        # real checkout): the target's parent is NOT an ancestor of main.
        self._base = tempfile.TemporaryDirectory(prefix="guard-55826e4f-")
        base = os.path.realpath(self._base.name)
        self.main = os.path.join(base, "main")
        os.makedirs(self.main)
        subprocess.run(["git", "init", "-q", self.main], check=True)
        self.scratch = os.path.join(base, "scratch")
        self.out = os.path.join(self.scratch, "condukt-wt-scope-abc")
        os.makedirs(self.out)

    def tearDown(self):
        self._base.cleanup()

    def test_literal_tmp_target_is_allowed(self):
        r = run_guard(self.main, f"rm -rf {self.out}")
        self.assertEqual(r.returncode, 0, f"literal out-of-tree target refused: {r.stderr}")
        r = run_guard(self.main, f"rm -rf {self.scratch}/condukt-wt-scope-*")
        self.assertEqual(r.returncode, 0, f"literal out-of-tree glob refused: {r.stderr}")

    @unittest.skip(
        "backlog f036e218 / 55826e4f OPEN: a target held in a shell variable that "
        "points outside the main tree is refused as a main-tree mutation"
    )
    def test_variable_tmp_target_is_allowed(self):
        r = run_guard(self.main, f"D={self.out}; rm -rf $D")
        self.assertEqual(r.returncode, 0, f"same-line variable target refused: {r.stderr}")


if __name__ == "__main__":
    unittest.main()
