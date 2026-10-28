#!/usr/bin/env python3
"""guard-maintree-bash blames a concurrent session's merge on the current command.

Observed 2026-10-26 (session c5b9c02b): the PostToolUse:Bash hook refused a
command that wrote only inside a scratchpad worktree, with

    Refused: this command modified this project's MAIN working tree outside
    integration (CLAUDE.md 8: main only receives merges).
    Changed in /Users/yuki/src/harness: .backlog/tasks.toml ...

During that call ANOTHER session merged into main: 807ca59e "backlog: add
775e5bc8" via merge 16f2e9c9 (`merge c5b9-sessionroots-red: Merge made by the
'ort' strategy.` in main's reflog). The message names no "HEAD a -> b" line, so
HEAD was the same at Pre and Post: the Post snapshot was taken while the other
session's `git merge` had already written the merged tree into main's index and
working tree but had not yet moved HEAD. That is the window in which git runs
the pre-merge-commit hook (this repo's .githooks/pre-merge-commit runs gates,
so the window is long). There is no MERGE_HEAD during it, so post()
(scripts/guard-maintree-bash.py:343-357) falls through to `verdict = REFUSE`
and prints "this command modified ...".

This test reproduces exactly that: the Post hook of an unrelated call (`true`)
is run from inside a scratch repo's pre-merge-commit hook of a concurrent
`git merge --no-ff`. It asserts the guard either allows, or at least does not
claim "this command modified" main (i.e. it tells "another session is merging
into main" apart from "this command modified main").

Controls (green today): a concurrent merge that COMPLETES between Pre and Post
is allowed; a command that really edits a tracked file in main is refused.

Never touches the real main tree: everything lives in a tempfile scratch repo.

    python3 scripts/test_guard_maintree_bash_concurrent_merge.py

Written by a test author who did not implement the guard (CLAUDE.md 2.(a)).
"""
from __future__ import annotations

import json
import os
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

MODIFIED_CLAIM = "this command modified this project's MAIN working tree"

HOOK = r'''#!/usr/bin/env python3
# pre-merge-commit of the CONCURRENT session's merge: at this point git has
# written the merged tree into main's index + working tree but HEAD has not
# moved. Run the OTHER session's PostToolUse guard now and record the result.
import json, os, subprocess, sys
from pathlib import Path
cfg = json.loads(Path(%(cfg)r).read_text())
env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
env["CLAUDE_PROJECT_DIR"] = cfg["main"]
def g(*a):
    return subprocess.run(["git", "-C", cfg["main"], *a], env=env,
                          capture_output=True, text=True).stdout
r = subprocess.run([sys.executable, cfg["guard"]], input=cfg["payload"],
                   env=env, capture_output=True, text=True, cwd=cfg["main"])
Path(cfg["out"]).write_text(json.dumps({
    "rc": r.returncode, "stderr": r.stderr,
    "head": g("rev-parse", "HEAD").strip(),
    "status": g("status", "--porcelain=v2", "--untracked-files=all"),
    "merge_head": Path(cfg["main"], ".git", "MERGE_HEAD").exists(),
}))
sys.exit(0)
'''


class ConcurrentMergeBase(ObserveGuardBase):
    SESSION = "sess-concurrent-merge-0001"

    def _side_branch_changes_tracked(self) -> None:
        (self.wt / "tracked.txt").write_text("v2 from the other session\n")
        git(self.wt, "commit", "-q", "-am", "backlog: add (other session)")

    def _head(self) -> str:
        return git(self.main, "rev-parse", "HEAD").stdout.strip()

    def _post_payload(self, tid: str, command: str) -> str:
        return json.dumps({
            "hook_event_name": "PostToolUse",
            "session_id": self.SESSION,
            "tool_use_id": tid,
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "cwd": str(self.wt),
            "tool_response": {"stdout": "", "stderr": "", "interrupted": False},
        })

    def _concurrent_merge(self, hook_cfg: dict | None) -> subprocess.CompletedProcess:
        """Another session runs `git merge --no-ff wtbranch` in main."""
        hooks = self.tmp / "hooks"
        hooks.mkdir(exist_ok=True)
        if hook_cfg is not None:
            cfgp = self.tmp / "hookcfg.json"
            cfgp.write_text(json.dumps(hook_cfg))
            h = hooks / "pre-merge-commit"
            h.write_text(HOOK % {"cfg": str(cfgp)})
            h.chmod(0o755)
        # A later -c wins over GIT_ID's core.hooksPath=/dev/null.
        return subprocess.run(
            ["git", *GIT_ID, "-c", "core.hooksPath=%s" % hooks, "merge", "-q",
             "--no-ff", "-m", "Merge branch 'wtbranch' (other session)", "wtbranch"],
            cwd=self.main, env=_clean_env(), capture_output=True, text=True)


class GuardConcurrentMerge(ConcurrentMergeBase):
    def test_red_post_inside_concurrent_merge_window_does_not_blame_command(self) -> None:
        self._side_branch_changes_tracked()
        old = self._head()
        cmd = "true"  # this call touches nothing at all
        tid, _ = self.pre_ok(cmd, cwd=self.wt)
        out = self.tmp / "hook-result.json"
        m = self._concurrent_merge({
            "main": str(self.main), "guard": str(SCRIPT), "out": str(out),
            "payload": self._post_payload(tid, cmd),
        })
        self.assertEqual(m.returncode, 0, "anti-vacuity: concurrent merge ran: %s" % m.stderr)
        self.assertTrue(out.exists(), "anti-vacuity: pre-merge-commit hook ran the guard")
        res = json.loads(out.read_text())
        # Anti-vacuity: the Post really happened in the window that was observed.
        self.assertEqual(res["head"], old, "anti-vacuity: HEAD not yet moved at Post")
        self.assertIn("tracked.txt", res["status"],
                      "anti-vacuity: merged file is dirty vs HEAD at Post")
        self.assertFalse(res["merge_head"], "anti-vacuity: no MERGE_HEAD in the window")
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(len(parents), 3, "anti-vacuity: the merge then completed")
        sys.stderr.write("\n[observed] guard rc=%d stderr:\n%s\n" % (res["rc"], res["stderr"]))
        self.assertTrue(
            res["rc"] == 0 or MODIFIED_CLAIM not in res["stderr"],
            "guard blamed another session's in-flight merge on this command "
            "(rc=%d): %s" % (res["rc"], res["stderr"]))

    def test_control_concurrent_merge_completed_between_pre_and_post_is_allowed(self) -> None:
        self._side_branch_changes_tracked()
        old = self._head()
        tid, _ = self.pre_ok("true", cwd=self.wt)
        m = self._concurrent_merge(None)
        self.assertEqual(m.returncode, 0, m.stderr)
        self.assertNotEqual(self._head(), old, "anti-vacuity: HEAD moved")
        r = self.post(tid, "true", cwd=self.wt)
        self.assertEqual(r.returncode, 0, r.stderr)

    def test_control_command_really_editing_main_is_refused(self) -> None:
        tid, _ = self.pre_ok("echo x > tracked.txt")
        (self.main / "tracked.txt").write_text("edited in main\n")
        r = self.post(tid, "echo x > tracked.txt")
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn(MODIFIED_CLAIM, r.stderr)


if __name__ == "__main__":
    prog = unittest.main(exit=False, verbosity=2)
    res = prog.result
    failed = len(res.failures) + len(res.errors)
    passed = res.testsRun - failed - len(res.skipped)
    print("RESULT: %d passed, %d failed" % (passed, failed))
    sys.exit(1 if failed else 0)
