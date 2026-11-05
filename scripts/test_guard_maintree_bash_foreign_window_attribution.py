#!/usr/bin/env python3
"""guard-maintree-bash: a foreign `git merge` window in main is only attributed
to ANOTHER process when attribution is determinable; otherwise it stays refused
(CLAUDE.md 3).

Companion to test_guard_maintree_bash_foreign_merge_window.py (which proves the
allow for a command whose Pre/Post cwd is a linked worktree and whose text
cannot point at main). Every case here runs the guard's Post inside a REAL
foreign `git merge --no-ff` pre-merge-commit window in main and asserts that an
undeterminable attribution (cwd missing / not a git work tree / main itself,
command text naming main or a redirect marker, Pre snapshot without `cwd`)
still refuses (rc 2). A positive case in the same harness proves the window is
one the guard would otherwise allow, so the refusals are not vacuous.

Written by the implementer of the attribution change (stated plainly, CLAUDE.md
2.(a)): an independent review of these cases is still owed.

    python3 -m pytest scripts/test_guard_maintree_bash_foreign_window_attribution.py
"""
from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_guard_maintree_bash_observe import (  # noqa: E402
    GIT_ID,
    SCRIPT,
    ObserveGuardBase,
    _clean_env,
    git,
)

HOOK = r'''#!/usr/bin/env python3
import json, os, subprocess, sys
from pathlib import Path
cfg = json.loads(Path(%(cfg)r).read_text())
env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
env["CLAUDE_PROJECT_DIR"] = cfg["main"]
pre = json.loads(Path(cfg["pre_snapshot"]).read_text()) if cfg.get("strip_pre_cwd") else None
if pre is not None:
    pre.pop("cwd", None)
    Path(cfg["pre_snapshot"]).write_text(json.dumps(pre))
r = subprocess.run([sys.executable, cfg["guard"]], input=cfg["payload"],
                   env=env, capture_output=True, text=True, cwd=cfg["main"])
Path(cfg["out"]).write_text(json.dumps({"rc": r.returncode, "stderr": r.stderr}))
sys.exit(0)
'''

OK_CMD = "cargo test -p blastguard"


class ForeignWindowAttribution(ObserveGuardBase):
    SESSION = "sess-foreign-attr-0001"

    def setUp(self) -> None:
        super().setUp()
        d = self.tmp / "wt-feature"
        git(self.main, "worktree", "add", "-q", "-b", "feature", str(d))
        (d / "feature.txt").write_text("feature\n")
        git(d, "add", "-A")
        git(d, "commit", "-q", "-m", "work feature")

    def _window(self, pre_cmd: str, pre_cwd: Path, post_cmd: str, post_cwd,
                strip_pre_cwd: bool = False) -> dict:
        tid, new = self.pre_ok(pre_cmd, cwd=pre_cwd)
        payload = {
            "hook_event_name": "PostToolUse", "session_id": self.SESSION,
            "tool_use_id": tid, "tool_name": "Bash",
            "tool_input": {"command": post_cmd},
            "tool_response": {"stdout": "", "stderr": "", "interrupted": False},
        }
        if post_cwd is not None:
            payload["cwd"] = post_cwd
        head0 = git(self.main, "rev-parse", "HEAD").stdout.strip()
        hooks = self.tmp / "hooks"
        hooks.mkdir()
        out = self.tmp / "hook-result.json"
        cfgp = self.tmp / "hookcfg.json"
        cfgp.write_text(json.dumps({
            "main": str(self.main), "guard": str(SCRIPT), "out": str(out),
            "payload": json.dumps(payload), "pre_snapshot": str(next(iter(new))),
            "strip_pre_cwd": strip_pre_cwd,
        }))
        h = hooks / "pre-merge-commit"
        h.write_text(HOOK % {"cfg": str(cfgp)})
        h.chmod(0o755)
        m = subprocess.run(
            ["git", *GIT_ID, "-c", "core.hooksPath=%s" % hooks, "merge", "-q",
             "--no-ff", "-m", "Merge feature", "feature"],
            cwd=self.main, env=_clean_env(), capture_output=True, text=True)
        self.assertTrue(out.exists(), "anti-vacuity: window hook ran the guard: " + m.stderr)
        self.assertNotEqual(git(self.main, "rev-parse", "HEAD").stdout.strip(), head0,
                            "anti-vacuity: the foreign merge really completed")
        return json.loads(out.read_text())

    def assert_refused(self, res: dict) -> None:
        self.assertEqual(res["rc"], 2, "undeterminable attribution must refuse:\n" + res["stderr"])
        self.assertIn("ORIG_HEAD was rewritten", res["stderr"])

    # positive control: same harness, attribution determinable -> allowed
    def test_control_linked_worktree_cwd_plain_command_allowed(self) -> None:
        res = self._window(OK_CMD, self.wt, OK_CMD, str(self.wt))
        self.assertEqual(res["rc"], 0, res["stderr"])

    def test_post_cwd_missing_refuses(self) -> None:
        self.assert_refused(self._window(OK_CMD, self.wt, OK_CMD, None))

    def test_post_cwd_not_a_string_refuses(self) -> None:
        self.assert_refused(self._window(OK_CMD, self.wt, OK_CMD, 42))

    def test_post_cwd_nonexistent_refuses(self) -> None:
        self.assert_refused(self._window(OK_CMD, self.wt, OK_CMD, str(self.tmp / "nope")))

    def test_post_cwd_not_a_git_work_tree_refuses(self) -> None:
        plain = self.tmp / "plain"
        plain.mkdir()
        self.assert_refused(self._window(OK_CMD, self.wt, OK_CMD, str(plain)))

    def test_pre_cwd_is_main_refuses(self) -> None:
        self.assert_refused(self._window(OK_CMD, self.main, OK_CMD, str(self.wt)))

    def test_post_cwd_is_main_subdir_refuses(self) -> None:
        sub = self.main / "sub"
        sub.mkdir()
        self.assert_refused(self._window(OK_CMD, self.wt, OK_CMD, str(sub)))

    def test_pre_snapshot_without_cwd_refuses(self) -> None:
        self.assert_refused(self._window(OK_CMD, self.wt, OK_CMD, str(self.wt),
                                         strip_pre_cwd=True))

    def test_command_naming_main_path_refuses(self) -> None:
        cmd = "ls %s" % self.main
        self.assert_refused(self._window(cmd, self.wt, cmd, str(self.wt)))

    def test_command_with_redirect_markers_refuses(self) -> None:
        for cmd in ("cd .. && git status", "git -C ../main status",
                    "git --git-dir=../main/.git status", "GIT_DIR=x git status",
                    "pushd ..", "make --directory=..",
                    "python3 -c 'import os; os.chdir(\"..\")'"):
            with self.subTest(cmd=cmd):
                self.tearDown_window()
                self.assert_refused(self._window(cmd, self.wt, cmd, str(self.wt)))

    def tearDown_window(self) -> None:
        """Reset main and the hook dir so _window can run again in one test."""
        hooks = self.tmp / "hooks"
        if hooks.exists():
            for f in hooks.iterdir():
                f.unlink()
            hooks.rmdir()
        if git(self.main, "rev-parse", "-q", "--verify", "ORIG_HEAD", check=False).returncode == 0:
            git(self.main, "reset", "-q", "--hard", "ORIG_HEAD")


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
