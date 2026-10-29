#!/usr/bin/env python3
"""guard-maintree-bash vs ONE Bash command that runs several `git merge --no-ff`.

Observed (session c5b9c02b): the Post hook refused a single Bash command that
looped over three `git merge --no-ff <branch> -F <msg>` in main:

    Refused: this command modified this project's MAIN working tree outside
    integration (CLAUDE.md 8: main only receives merges). Changed in ...:
    HEAD d51cebe3e773 -> a8b441c5c990 (not a merge / fast-forward)

although a8b441c5 is a two-parent merge of d51cebe3. The command had exceeded the
Bash 600s timeout and been moved to the background, so Post may have fired while
a LATER merge was still inside its pre-merge-commit hook (prediction, not
observation). Two shapes are driven here:

  (1) several sequential merges all COMPLETE between Pre and Post, clean tree.
  (2) Post is taken from inside the pre-merge-commit hook of the 2nd merge:
      HEAD = 1st merge commit (!= Pre HEAD), 2nd branch's tree already in
      index + worktree, no MERGE_HEAD yet.

Fixture repos live under a temp dir; git runs with inherited GIT_* removed.

    python3 scripts/test_guard_maintree_bash_multi_merge.py

Written by a test author who did not implement the guard (CLAUDE.md 2.(a)).
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

BRANCHES = ["mb-one", "mb-two", "mb-three"]

# pre-merge-commit: on the SECOND merge only, run the guard's Post in the window.
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


class MultiMerge(ObserveGuardBase):
    SESSION = "sess-multi-merge-0001"
    CMD = "for b in mb-one mb-two mb-three; do git merge --no-ff $b -F msg.txt; done"

    def setUp(self) -> None:
        super().setUp()
        for b in BRANCHES:
            d = self.tmp / ("wt-" + b)
            git(self.main, "worktree", "add", "-q", "-b", b, str(d))
            (d / (b + ".txt")).write_text(b + "\n")
            git(d, "add", "-A")
            git(d, "commit", "-q", "-m", "work " + b)

    def _head(self) -> str:
        return git(self.main, "rev-parse", "HEAD").stdout.strip()

    def _merge(self, branch: str, hooks: Path | None) -> subprocess.CompletedProcess:
        extra = ["-c", "core.hooksPath=%s" % hooks] if hooks else []
        return subprocess.run(
            ["git", *GIT_ID, *extra, "merge", "-q", "--no-ff", "-m",
             "Merge " + branch, branch],
            cwd=self.main, env=_clean_env(), capture_output=True, text=True)

    def _post_payload(self, tid: str) -> str:
        return json.dumps({
            "hook_event_name": "PostToolUse", "session_id": self.SESSION,
            "tool_use_id": tid, "tool_name": "Bash",
            "tool_input": {"command": self.CMD}, "cwd": str(self.main),
            "tool_response": {"stdout": "", "stderr": "", "interrupted": False},
        })

    def test_shape1_sequential_merges_completed_between_pre_and_post(self) -> None:
        old = self._head()
        tid, _ = self.pre_ok(self.CMD)
        for b in BRANCHES:
            m = self._merge(b, None)
            self.assertEqual(m.returncode, 0, m.stderr)
        new = self._head()
        self.assertNotEqual(new, old, "anti-vacuity: HEAD moved")
        n_merges = git(self.main, "rev-list", "--merges", "--count",
                       "%s..%s" % (old, new)).stdout.strip()
        self.assertEqual(n_merges, "3", "anti-vacuity: three merge commits")
        self.assertEqual(self.status(), "", "anti-vacuity: tree clean at Post")
        r = self.post(tid, self.CMD)
        self.assertEqual(r.returncode, 0,
                         "guard refused %d sequential completed merges: %s" % (3, r.stderr))

    def test_shape2_post_inside_second_merge_pre_merge_commit_window(self) -> None:
        old = self._head()
        tid, _ = self.pre_ok(self.CMD)
        m1 = self._merge(BRANCHES[0], None)
        self.assertEqual(m1.returncode, 0, m1.stderr)
        hooks = self.tmp / "hooks"
        hooks.mkdir()
        out = self.tmp / "hook-result.json"
        cfgp = self.tmp / "hookcfg.json"
        cfgp.write_text(json.dumps({
            "main": str(self.main), "guard": str(SCRIPT), "out": str(out),
            "counter": str(self.tmp / "counter"), "payload": self._post_payload(tid),
        }))
        # counter semantic: hook run #1 (merge 2) is the one that must fire; make
        # the hook count from 1 for merge 2 by pre-seeding the counter at 1.
        (self.tmp / "counter").write_text("1")
        h = hooks / "pre-merge-commit"
        h.write_text(HOOK % {"cfg": str(cfgp)})
        h.chmod(0o755)
        m2 = self._merge(BRANCHES[1], hooks)
        self.assertEqual(m2.returncode, 0, m2.stderr)
        self.assertTrue(out.exists(), "anti-vacuity: hook ran the guard Post")
        res = json.loads(out.read_text())
        self.assertNotEqual(res["head"], old, "anti-vacuity: HEAD = 1st merge, moved vs Pre")
        self.assertIn("mb-two.txt", res["status"],
                      "anti-vacuity: 2nd branch's file in index/worktree at Post")
        self.assertFalse(res["merge_head"], "anti-vacuity: no MERGE_HEAD in the window")
        parents = git(self.main, "rev-list", "--parents", "-n1", res["head"]).stdout.split()
        self.assertEqual(len(parents), 3, "anti-vacuity: HEAD at Post is a merge commit")
        sys.stderr.write("\n[observed] guard rc=%d stderr:\n%s\n" % (res["rc"], res["stderr"]))
        self.assertEqual(res["rc"], 0,
                         "guard refused Post taken mid-way through a multi-merge "
                         "command: %s" % res["stderr"])


if __name__ == "__main__":
    prog = unittest.main(exit=False, verbosity=2)
    res = prog.result
    failed = len(res.failures) + len(res.errors)
    passed = res.testsRun - failed - len(res.skipped)
    print("RESULT: %d passed, %d failed" % (passed, failed))
    sys.exit(1 if failed else 0)
