#!/usr/bin/env python3
"""guard-maintree-bash in_flight_merge_window: what it must still REFUSE, and the
documented residual (backlog 2dee8b0e).

After an integration HEAD move (a first `git merge --no-ff`), the guard accepts a
dirty tree only if it is exactly a later merge caught in its pre-merge-commit
window. These tests pin the near-misses that must stay refused, and the one
documented residual (`git reset -q HEAD` + `git cherry-pick -n <one-commit
branch>`), including that the residual never turns into a COMMIT on main.

    python3 scripts/test_guard_maintree_bash_window_refusals.py

Written by a verifier who did not implement the guard (CLAUDE.md 2.(a)).
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

# pre-merge-commit of the 2nd merge: optionally dirty the tree, then run the guard Post.
HOOK = r'''#!/usr/bin/env python3
import json, os, subprocess, sys
from pathlib import Path
cfg = json.loads(Path(%(cfg)r).read_text())
counter = Path(cfg["counter"])
n = int(counter.read_text()) + 1 if counter.exists() else 1
counter.write_text(str(n))
if n != 2:
    sys.exit(0)
env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
env["CLAUDE_PROJECT_DIR"] = cfg["main"]
main = Path(cfg["main"])
mode = cfg["mode"]
if mode == "unstaged":
    (main / "other.txt").write_text("edited in window\n")
elif mode == "untracked":
    (main / "newfile.txt").write_text("new in window\n")
elif mode == "staged":
    (main / "other.txt").write_text("staged in window\n")
    subprocess.run(["git", "-C", cfg["main"], "add", "other.txt"], env=env, check=True)
r = subprocess.run([sys.executable, cfg["guard"]], input=cfg["payload"],
                   env=env, capture_output=True, text=True, cwd=cfg["main"])
Path(cfg["out"]).write_text(json.dumps({"rc": r.returncode, "stderr": r.stderr}))
sys.exit(0)
'''


class WindowRefusals(ObserveGuardBase):
    SESSION = "sess-window-refusals-0001"
    CMD = "git merge --no-ff mb-one; git merge --no-ff mb-two"

    def setUp(self) -> None:
        super().setUp()
        for b in ("mb-one", "mb-two"):
            d = self.tmp / ("wt-" + b)
            git(self.main, "worktree", "add", "-q", "-b", b, str(d))
            (d / (b + ".txt")).write_text(b + "\n")
            git(d, "add", "-A")
            git(d, "commit", "-q", "-m", "work " + b)

    def head(self) -> str:
        return git(self.main, "rev-parse", "HEAD").stdout.strip()

    def merge_one(self) -> None:
        m = git(self.main, "merge", "-q", "--no-ff", "-m", "Merge mb-one", "mb-one")
        self.assertEqual(m.returncode, 0, m.stderr)

    def post_payload(self, tid: str) -> str:
        return json.dumps({
            "hook_event_name": "PostToolUse", "session_id": self.SESSION,
            "tool_use_id": tid, "tool_name": "Bash",
            "tool_input": {"command": self.CMD}, "cwd": str(self.main),
            "tool_response": {"stdout": "", "stderr": "", "interrupted": False},
        })

    def window(self, mode: str) -> dict:
        tid, _ = self.pre_ok(self.CMD)
        old = self.head()
        self.merge_one()
        self.assertNotEqual(self.head(), old, "anti-vacuity: HEAD moved by merge 1")
        hooks = self.tmp / "hooks"
        hooks.mkdir()
        out = self.tmp / "hook-result.json"
        cfgp = self.tmp / "hookcfg.json"
        cfgp.write_text(json.dumps({
            "main": str(self.main), "guard": str(SCRIPT), "out": str(out),
            "counter": str(self.tmp / "counter"), "payload": self.post_payload(tid),
            "mode": mode,
        }))
        (self.tmp / "counter").write_text("1")
        h = hooks / "pre-merge-commit"
        h.write_text(HOOK % {"cfg": str(cfgp)})
        h.chmod(0o755)
        m2 = subprocess.run(
            ["git", *GIT_ID, "-c", "core.hooksPath=%s" % hooks, "merge", "-q",
             "--no-ff", "-m", "Merge mb-two", "mb-two"],
            cwd=self.main, env=_clean_env(), capture_output=True, text=True)
        self.assertTrue(out.exists(), "anti-vacuity: window hook ran the guard: " + m2.stderr)
        return json.loads(out.read_text())

    # -- baseline: the pure window is allowed (so the refusals below are not vacuous)
    def test_pure_window_allowed(self) -> None:
        res = self.window("none")
        self.assertEqual(res["rc"], 0, res["stderr"])

    def test_window_plus_unstaged_edit_refused(self) -> None:
        res = self.window("unstaged")
        self.assertEqual(res["rc"], 2, "window + unstaged edit must be refused")
        self.assertIn("other.txt", res["stderr"])

    def test_window_plus_untracked_file_refused(self) -> None:
        res = self.window("untracked")
        self.assertEqual(res["rc"], 2, "window + new untracked file must be refused")
        self.assertIn("newfile.txt", res["stderr"])

    def test_window_plus_staged_edit_refused(self) -> None:
        res = self.window("staged")
        self.assertEqual(res["rc"], 2, "window + staged edit must be refused")
        self.assertIn("other.txt", res["stderr"])

    # -- after an integration HEAD move, non-window dirty states
    def test_merge_then_squash_refused(self) -> None:
        tid, _ = self.pre_ok(self.CMD)
        self.merge_one()
        s = git(self.main, "merge", "--squash", "mb-two")
        self.assertEqual(s.returncode, 0, s.stderr)
        self.assertIn("mb-two.txt", self.status(), "anti-vacuity: squash staged")
        self.assertFalse((self.main / ".git" / "MERGE_HEAD").exists())
        self.assertEqual(self.post(tid, self.CMD).returncode, 2)

    def test_merge_then_cherry_pick_n_without_reset_refused(self) -> None:
        tid, _ = self.pre_ok(self.CMD)
        self.merge_one()
        c = git(self.main, "cherry-pick", "-n", "mb-two")
        self.assertEqual(c.returncode, 0, c.stderr)
        self.assertIn("mb-two.txt", self.status(), "anti-vacuity: cherry-pick staged")
        self.assertEqual(self.post(tid, self.CMD).returncode, 2)

    def test_merge_then_staged_edit_refused(self) -> None:
        tid, _ = self.pre_ok(self.CMD)
        self.merge_one()
        (self.main / "other.txt").write_text("sneaky\n")
        git(self.main, "add", "other.txt")
        self.assertIn("other.txt", self.status(), "anti-vacuity: edit staged")
        self.assertEqual(self.post(tid, self.CMD).returncode, 2)

    # -- the documented residual and its limits
    def test_residual_reset_plus_cherry_pick_n_state_and_no_commit(self) -> None:
        """Documented residual: allowed, but ONLY as an uncommitted state."""
        tid, _ = self.pre_ok(self.CMD)
        self.merge_one()
        merged = self.head()
        git(self.main, "reset", "-q", "HEAD")
        c = git(self.main, "cherry-pick", "-n", "mb-two")
        self.assertEqual(c.returncode, 0, c.stderr)
        self.assertIn("mb-two.txt", self.status(), "anti-vacuity: staged")
        r = self.post(tid, self.CMD)
        sys.stderr.write("\n[residual] guard rc=%d stderr=%r\n" % (r.returncode, r.stderr))
        self.assertEqual(self.head(), merged, "no commit was created by the residual path")
        self.assertEqual(r.returncode, 0, "residual currently documented as allowed")

    def test_residual_followed_by_commit_in_next_call_refused(self) -> None:
        # call 1 leaves the residual state (allowed); call 2 commits it as a
        # NON-merge commit on main -> must be refused.
        tid1, _ = self.pre_ok(self.CMD)
        self.merge_one()
        git(self.main, "reset", "-q", "HEAD")
        git(self.main, "cherry-pick", "-n", "mb-two")
        self.assertEqual(self.post(tid1, self.CMD).returncode, 0)
        before = self.head()
        tid2, _ = self.pre_ok("git commit -m x")
        c = git(self.main, "commit", "-q", "-m", "sneaky non-merge commit")
        self.assertEqual(c.returncode, 0, c.stderr)
        self.assertNotEqual(self.head(), before, "anti-vacuity: commit landed on main")
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(len(parents), 2, "anti-vacuity: non-merge commit")
        self.assertEqual(self.post(tid2, "git commit -m x").returncode, 2)

    def test_residual_same_call_commit_refused(self) -> None:
        tid, _ = self.pre_ok(self.CMD)
        self.merge_one()
        git(self.main, "reset", "-q", "HEAD")
        c = git(self.main, "cherry-pick", "mb-two")  # full commit, non-merge
        self.assertEqual(c.returncode, 0, c.stderr)
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(len(parents), 2, "anti-vacuity: non-merge commit on main")
        self.assertEqual(self.post(tid, self.CMD).returncode, 2)


if __name__ == "__main__":
    prog = unittest.main(exit=False, verbosity=2)
    res = prog.result
    failed = len(res.failures) + len(res.errors)
    sys.exit(1 if failed else 0)
