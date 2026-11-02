#!/usr/bin/env python3
"""guard-maintree-bash: a command that never touches main must not be refused
because ANOTHER process's `git merge` is in flight in main (backlog: foreign
merge window, successor to 2dee8b0e / in_flight_merge_window).

Observed 2026-11-01 ~01:18: an agent's Bash call `cargo test --manifest-path
<its own worktree>/Cargo.toml -p blastguard` was refused with "ORIG_HEAD was
rewritten during this call while HEAD did not move and there is no MERGE_HEAD"
because a different agent's `git merge --no-ff` into main was in its
pre-merge-commit window (ORIG_HEAD rewritten, merged tree staged, HEAD unmoved,
no MERGE_HEAD yet) between this call's Pre and Post.

    python3 -m pytest scripts/test_guard_maintree_bash_foreign_merge_window.py

The "foreign merge" is a real `git merge --no-ff` run by the test (not by the
command under guard); a pre-merge-commit hook runs the guard's Post inside the
window, exactly as the existing window tests do.

Known limit (stated plainly): from on-disk state alone the foreign window and a
window produced by the guarded command itself are byte-identical, so the
control below uses a DIFFERENT own-write shape (a staged edit + `git reset`
inside main that is complete before Post), which the guard must keep refusing.
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
r = subprocess.run([sys.executable, cfg["guard"]], input=cfg["payload"],
                   env=env, capture_output=True, text=True, cwd=cfg["main"])
Path(cfg["out"]).write_text(json.dumps({"rc": r.returncode, "stderr": r.stderr}))
sys.exit(0)
'''


class ForeignMergeWindow(ObserveGuardBase):
    SESSION = "sess-foreign-window-0001"
    CMD = "cargo test --manifest-path /some/own/worktree/Cargo.toml -p blastguard"

    def setUp(self) -> None:
        super().setUp()
        d = self.tmp / "wt-feature"
        git(self.main, "worktree", "add", "-q", "-b", "feature", str(d))
        (d / "feature.txt").write_text("feature\n")
        git(d, "add", "-A")
        git(d, "commit", "-q", "-m", "work feature")

    def post_payload(self, tid: str) -> str:
        return json.dumps({
            "hook_event_name": "PostToolUse", "session_id": self.SESSION,
            "tool_use_id": tid, "tool_name": "Bash",
            "tool_input": {"command": self.CMD}, "cwd": str(self.tmp / "wt"),
            "tool_response": {"stdout": "", "stderr": "", "interrupted": False},
        })

    def test_foreign_merge_window_does_not_refuse_unrelated_command(self) -> None:
        tid, _ = self.pre_ok(self.CMD, cwd=self.wt)  # command runs in its own worktree
        head0 = git(self.main, "rev-parse", "HEAD").stdout.strip()
        hooks = self.tmp / "hooks"
        hooks.mkdir()
        out = self.tmp / "hook-result.json"
        cfgp = self.tmp / "hookcfg.json"
        cfgp.write_text(json.dumps({
            "main": str(self.main), "guard": str(SCRIPT), "out": str(out),
            "payload": self.post_payload(tid),
        }))
        h = hooks / "pre-merge-commit"
        h.write_text(HOOK % {"cfg": str(cfgp)})
        h.chmod(0o755)
        # ANOTHER process merges in main; the guarded command does nothing there.
        m = subprocess.run(
            ["git", *GIT_ID, "-c", "core.hooksPath=%s" % hooks, "merge", "-q",
             "--no-ff", "-m", "Merge feature", "feature"],
            cwd=self.main, env=_clean_env(), capture_output=True, text=True)
        self.assertTrue(out.exists(), "anti-vacuity: window hook ran the guard: " + m.stderr)
        res = json.loads(out.read_text())
        # anti-vacuity: the window really had the claimed shape when Post ran
        # (the merge has completed by now, so check what the guard saw via stderr
        # only when refused; HEAD must have moved only AFTER Post ran).
        self.assertNotEqual(git(self.main, "rev-parse", "HEAD").stdout.strip(), head0,
                            "anti-vacuity: the foreign merge really completed")
        self.assertEqual(res["rc"], 0,
                         "command never touched main but was refused:\n" + res["stderr"])

    def test_control_own_write_in_main_still_refused(self) -> None:
        tid, _ = self.pre_ok("git reset -q && edit", cwd=self.main)
        (self.main / "other.txt").write_text("own edit\n")
        git(self.main, "add", "other.txt")
        git(self.main, "reset", "-q", "HEAD")  # rewrites ORIG_HEAD, HEAD unmoved
        self.assertTrue((self.main / ".git" / "ORIG_HEAD").exists(), "anti-vacuity")
        self.assertIn("other.txt", self.status(), "anti-vacuity: change landed in main")
        r = self.post(tid, "git reset -q && edit")
        self.assertEqual(r.returncode, 2, "own write into main must stay refused")
        self.assertIn("other.txt", r.stderr)


if __name__ == "__main__":
    sys.exit(0 if unittest.main(exit=False).result.wasSuccessful() else 1)
