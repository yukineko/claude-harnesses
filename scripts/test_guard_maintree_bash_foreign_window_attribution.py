#!/usr/bin/env python3
"""guard-maintree-bash: a foreign `git merge` window in main, judged by the PRE
cwd alone.

RE-SCOPED by user ruling 2026-11-08 ("donegateそのものが他のworktreeを見たり検証
したりテストしたりすることは禁止とする。他のgateでも禁止とする。合理的理由があって
確認するなら、それを提示すること。基本的に認めない" / "...定期的にまとまったauditの
実施での担保とする。"). The old contract attributed a foreign window to ANOTHER
process only when Pre/Post cwd and command text proved the call could not
target main (command_cannot_target_main). That attribution was deleted: a call
whose PRE cwd is a linked worktree is no longer judged against main at all
(Post cwd, command text and redirect markers are irrelevant), and a call whose
PRE cwd is main is still refused inside a foreign window with the AMBIGUOUS
ORIG_HEAD wording. Every case runs the guard's Post inside a REAL foreign
`git merge --no-ff` pre-merge-commit window in main.

What this does NOT prove: that a worktree-cwd command which really writes main
is caught — it is not (accepted detection loss, dropped with no compensating
mechanism).

Originally written by the implementer of the attribution change; re-scoped by
the implementer of the 2026-11-08 change (stated plainly, CLAUDE.md 2.(a)).

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
env["HOME"] = str(Path(cfg["main"]).parent / "home")  # sandbox the write record
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

    def assert_not_judged(self, res: dict) -> None:
        self.assertEqual(res["rc"], 0, "a worktree-cwd call is not judged against main:\n"
                         + res["stderr"])

    def test_linked_worktree_cwd_plain_command_allowed(self) -> None:
        self.assert_not_judged(self._window(OK_CMD, self.wt, OK_CMD, str(self.wt)))

    def test_pre_cwd_worktree_post_cwd_anything_allowed(self) -> None:
        """Only the PRE cwd decides; Post cwd (missing, wrong type, gone, not a
        tree, main's subdir) no longer makes the call judged."""
        plain = self.tmp / "plain"
        plain.mkdir()
        sub = self.main / "sub"
        sub.mkdir()
        for post_cwd in (None, 42, str(self.tmp / "nope"), str(plain), str(sub)):
            with self.subTest(post_cwd=post_cwd):
                self.tearDown_window()
                self.assert_not_judged(self._window(OK_CMD, self.wt, OK_CMD, post_cwd))

    def test_pre_cwd_is_main_refuses(self) -> None:
        """Main-cwd calls are still observed: a foreign window landing in this
        call's Pre/Post is refused with the ambiguous wording (CLAUDE.md 3)."""
        self.assert_refused(self._window(OK_CMD, self.main, OK_CMD, str(self.wt)))

    def test_pre_cwd_main_subdir_refuses(self) -> None:
        sub = self.main / "sub"
        sub.mkdir()
        self.assert_refused(self._window(OK_CMD, sub, OK_CMD, str(sub)))

    def test_command_text_is_irrelevant_from_a_worktree_cwd(self) -> None:
        """Accepted detection loss: command text that names main or redirects
        to it is not consulted for a worktree-cwd call."""
        for cmd in ("ls %s" % self.main, "cd .. && git status", "git -C ../main status",
                    "git --git-dir=../main/.git status", "GIT_DIR=x git status",
                    "pushd ..", "make --directory=..",
                    "python3 -c 'import os; os.chdir(\"..\")'"):
            with self.subTest(cmd=cmd):
                self.tearDown_window()
                self.assert_not_judged(self._window(cmd, self.wt, cmd, str(self.wt)))

    def tearDown_window(self) -> None:
        """Reset main and the hook dir so _window can run again in one test."""
        hooks = self.tmp / "hooks"
        if hooks.exists():
            for f in hooks.iterdir():
                f.unlink()
            hooks.rmdir()
        out = self.tmp / "hook-result.json"
        if out.exists():
            out.unlink()  # so _window's anti-vacuity check sees THIS run's result
        if git(self.main, "rev-parse", "-q", "--verify", "ORIG_HEAD", check=False).returncode == 0:
            git(self.main, "reset", "-q", "--hard", "ORIG_HEAD")


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
