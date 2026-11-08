#!/usr/bin/env python3
"""guard-maintree-bash charges ANOTHER session's merge into main to a main-cwd call.

Observed 2026-11-08 (session 32935915, subagent hand-back): a subagent's Bash
call whose hook payload cwd was the main tree (agent cwd is reset to the
project dir between calls) was refused at PostToolUse with

    Refused: this command modified this project's MAIN working tree outside
    integration (CLAUDE.md 8: main only receives merges).
    Changed in /Users/yuki/src/harness:
      HEAD a7856f82675d -> 38b17a9a1830 (not classified as merge or
      fast-forward: the working tree / index also changed during this call
      (paths below), so the HEAD move was refused without checking it)
      .claude-plugin/marketplace.json
      Cargo.lock
      crates/blastguard/...

38b17a9a is "Merge branch 'blastguard-stash-drop-allow'", made by a DIFFERENT
actor (the parent session) in main while the subagent's call ran. The shape
"HEAD moved to a real merge commit AND paths changed" is what a Pre snapshot
taken inside the other merge's pre-merge-commit window looks like: at Pre git
had already written the merged tree into main's index + working tree (paths
dirty vs the old HEAD), at Post the merge commit existed and the tree was
clean. The dirty-path sets differ, so post() never consults integration_move.

This file reproduces that with a scratch repo (never the real main):
  * session A (cwd = main) runs a harmless read-only command (`git status`);
  * session B (a different session id) runs a real `git merge --no-ff` into
    main, with its own Pre/Post guard calls, during A's call;
  * A must not be refused because of B's merge.
Variants: B's merge completes wholly between A's Pre and Post; A's Pre falls in
B's pre-merge-commit window (the observed shape); A's Post falls in it; B's
merge falls between two separate A calls; B fast-forwards main. Exception:
when A's Post falls in B's window (test_3) the evidence is an ambiguous
ORIG_HEAD rewrite and A stays REFUSED (user ruling 2026-11-08).

Controls that MUST still refuse (so the allow cases prove something): a direct
non-merge commit on main, a `git reset --hard` on main, and A's own write into
main made while B's merge completes in the same call.

    python3 scripts/test_guard_maintree_bash_concurrent_merge_main_cwd.py

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

SESSION_A = "sess-concurrent-main-cwd-A"
SESSION_B = "sess-concurrent-main-cwd-B"
A_CMD = "git status"  # harmless, read-only

# pre-merge-commit of session B's merge: git has written the merged tree into
# main's index + working tree, HEAD has not moved. Run ONE guard event of
# session A now (from main's cwd) and record the result.
HOOK = r'''#!/usr/bin/env python3
import json, os, subprocess, sys
from pathlib import Path
cfg = json.loads(Path(%(cfg)r).read_text())
env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
env["CLAUDE_PROJECT_DIR"] = cfg["main"]
env["HOME"] = cfg["home"]
def g(*a):
    return subprocess.run(["git", "-C", cfg["main"], *a], env=env,
                          capture_output=True, text=True).stdout
r = subprocess.run([sys.executable, cfg["guard"]], input=cfg["payload"],
                   env=env, capture_output=True, text=True, cwd=cfg["main"])
Path(cfg["out"]).write_text(json.dumps({
    "rc": r.returncode, "stderr": r.stderr,
    "head": g("rev-parse", "HEAD").strip(),
    "status": g("status", "--porcelain=v2", "--untracked-files=all"),
}))
sys.exit(0)
'''


class ConcurrentMergeMainCwdBase(ObserveGuardBase):
    SESSION = SESSION_A

    def _head(self) -> str:
        return git(self.main, "rev-parse", "HEAD").stdout.strip()

    def _side_branch_commit(self) -> None:
        """Session B's work on its own branch: a tracked change and a new file."""
        (self.wt / "tracked.txt").write_text("v2 from session B\n")
        (self.wt / "b_new.txt").write_text("b\n")
        git(self.wt, "add", "tracked.txt", "b_new.txt")
        git(self.wt, "commit", "-q", "-m", "session B work")

    def _main_side_commit(self) -> None:
        """Make main diverge so B's merge is a real merge commit."""
        (self.main / "m.txt").write_text("m\n")
        git(self.main, "add", "m.txt")
        git(self.main, "commit", "-q", "-m", "main side")

    def _payload(self, event: str, tid: str, session: str, command: str) -> str:
        p = {
            "hook_event_name": event,
            "session_id": session,
            "tool_use_id": tid,
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "cwd": str(self.main),
        }
        if event == "PostToolUse":
            p["tool_response"] = {"stdout": "", "stderr": "", "interrupted": False}
        return json.dumps(p)

    def _b_merge(self, hook_cfg: dict | None, ff: bool = False) -> subprocess.CompletedProcess:
        """Session B merges wtbranch into main, wrapped in B's own guard calls
        (cwd main, session B). B's own guard must allow its merge."""
        b_cmd = "git merge --ff-only wtbranch" if ff else "git merge --no-ff wtbranch"
        b_tid = self._next_id()
        r = self.run_hook("PreToolUse", b_tid, b_cmd, session=SESSION_B)
        self.assertEqual(r.returncode, 0, "anti-vacuity: B's Pre ran: %s" % r.stderr)
        hooks = self.tmp / "hooks"
        hooks.mkdir(exist_ok=True)
        if hook_cfg is not None:
            cfgp = self.tmp / "hookcfg.json"
            cfgp.write_text(json.dumps(hook_cfg))
            h = hooks / "pre-merge-commit"
            h.write_text(HOOK % {"cfg": str(cfgp)})
            h.chmod(0o755)
        args = ["--ff-only"] if ff else ["--no-ff", "-m", "Merge branch 'wtbranch' (session B)"]
        # A later -c wins over GIT_ID's core.hooksPath=/dev/null.
        m = subprocess.run(
            ["git", *GIT_ID, "-c", "core.hooksPath=%s" % hooks, "merge", "-q", *args,
             "wtbranch"],
            cwd=self.main, env=_clean_env(), capture_output=True, text=True)
        self.assertEqual(m.returncode, 0, "anti-vacuity: B's merge ran: %s" % m.stderr)
        rb = self.post(b_tid, b_cmd, session=SESSION_B)
        self.assertEqual(rb.returncode, 0,
                         "anti-vacuity: B's own guard allows B's merge: %s" % rb.stderr)
        return m

    def _hook_cfg(self, payload: str, out: Path) -> dict:
        return {"main": str(self.main), "guard": str(SCRIPT), "out": str(out),
                "home": str(self.tmp / "home"), "payload": payload}

    def _assert_merge_commit_landed(self, old: str) -> None:
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(len(parents), 3, "anti-vacuity: B's merge commit landed")
        self.assertEqual(parents[1], old, "anti-vacuity: first parent is old main HEAD")
        self.assertEqual(self.status(), "", "anti-vacuity: main clean after B's merge")

    def _record(self, session: str) -> str:
        p = self.tmp / "home" / ".claude" / "state" / "maintree-writes" / ("%s.jsonl" % session)
        return p.read_text() if p.exists() else ""

    def _assert_a_not_refused(self, r_rc: int, r_stderr: str) -> None:
        sys.stderr.write("\n[observed] A guard rc=%d stderr:\n%s\n" % (r_rc, r_stderr))
        self.assertEqual(
            r_rc, 0,
            "session A's harmless `%s` (cwd main) was refused because of session "
            "B's merge (rc=%d): %s" % (A_CMD, r_rc, r_stderr))


class ConcurrentMergeDoesNotRefuseMainCwdCall(ConcurrentMergeMainCwdBase):
    def test_1_b_merge_completes_between_a_pre_and_post(self) -> None:
        self._side_branch_commit()
        self._main_side_commit()
        old = self._head()
        tid, _ = self.pre_ok(A_CMD)
        self._b_merge(None)
        self._assert_merge_commit_landed(old)
        r = self.post(tid, A_CMD)
        self._assert_a_not_refused(r.returncode, r.stderr)

    def test_2_a_pre_inside_b_merge_window_post_after_completion(self) -> None:
        """The observed shape (HEAD a7856f82 -> 38b17a9a with paths changed)."""
        self._side_branch_commit()
        self._main_side_commit()
        old = self._head()
        tid = self._next_id()
        out = self.tmp / "a-pre.json"
        self._b_merge(self._hook_cfg(self._payload("PreToolUse", tid, SESSION_A, A_CMD), out))
        self.assertTrue(out.exists(), "anti-vacuity: A's Pre ran inside B's window")
        res = json.loads(out.read_text())
        self.assertEqual(res["rc"], 0, "anti-vacuity: A's Pre allowed: %s" % res["stderr"])
        self.assertEqual(res["head"], old, "anti-vacuity: HEAD not yet moved at A's Pre")
        self.assertIn("tracked.txt", res["status"],
                      "anti-vacuity: B's merged path dirty vs HEAD at A's Pre")
        self._assert_merge_commit_landed(old)
        r = self.post(tid, A_CMD)
        self._assert_a_not_refused(r.returncode, r.stderr)

    def test_2b_b_merge_paths_not_charged_to_a_write_record(self) -> None:
        """Same as test_2; A's write record must not name B's merged paths."""
        self._side_branch_commit()
        self._main_side_commit()
        tid = self._next_id()
        out = self.tmp / "a-pre.json"
        self._b_merge(self._hook_cfg(self._payload("PreToolUse", tid, SESSION_A, A_CMD), out))
        self.assertTrue(out.exists(), "anti-vacuity: A's Pre ran inside B's window")
        self.post(tid, A_CMD)
        rec = self._record(SESSION_A)
        sys.stderr.write("\n[observed] A write record:\n%s\n" % rec)
        self.assertNotIn("tracked.txt", rec,
                         "session B's merged path was charged to session A's write record")
        self.assertNotIn("b_new.txt", rec,
                         "session B's merged path was charged to session A's write record")

    def test_3_a_post_inside_b_merge_window(self) -> None:
        """User ruling 2026-11-08: an ORIG_HEAD rewrite with HEAD unmoved and no
        MERGE_HEAD is ambiguous (this command's own reset/merge/rebase produces
        it too), so it stays REFUSED (CLAUDE.md 3), with the AMBIGUOUS wording."""
        self._side_branch_commit()
        self._main_side_commit()
        old = self._head()
        tid, _ = self.pre_ok(A_CMD)
        out = self.tmp / "a-post.json"
        self._b_merge(self._hook_cfg(self._payload("PostToolUse", tid, SESSION_A, A_CMD), out))
        self.assertTrue(out.exists(), "anti-vacuity: A's Post ran inside B's window")
        res = json.loads(out.read_text())
        self.assertEqual(res["head"], old, "anti-vacuity: HEAD not yet moved at A's Post")
        self.assertIn("tracked.txt", res["status"],
                      "anti-vacuity: B's merged path dirty vs HEAD at A's Post")
        self._assert_merge_commit_landed(old)
        sys.stderr.write("\n[observed] A guard rc=%d stderr:\n%s\n" % (res["rc"], res["stderr"]))
        self.assertEqual(res["rc"], 2,
                         "ambiguous ORIG_HEAD rewrite must stay refused: %s" % res["stderr"])
        self.assertIn("ORIG_HEAD was rewritten during this call while HEAD did "
                      "not move and there is no MERGE_HEAD", res["stderr"])
        self.assertIn("NOT asserted to be this command's", res["stderr"])

    def test_4_b_merge_between_two_separate_a_calls(self) -> None:
        self._side_branch_commit()
        self._main_side_commit()
        old = self._head()
        tid, _ = self.pre_ok(A_CMD)
        r1 = self.post(tid, A_CMD)
        self.assertEqual(r1.returncode, 0, "anti-vacuity: A's first call allowed: %s" % r1.stderr)
        self._b_merge(None)
        self._assert_merge_commit_landed(old)
        tid2, _ = self.pre_ok(A_CMD)
        r2 = self.post(tid2, A_CMD)
        self._assert_a_not_refused(r2.returncode, r2.stderr)

    def test_5_b_fast_forward_between_a_pre_and_post(self) -> None:
        self._side_branch_commit()
        old = self._head()
        tid, _ = self.pre_ok(A_CMD)
        self._b_merge(None, ff=True)
        self.assertNotEqual(self._head(), old, "anti-vacuity: main fast-forwarded")
        self.assertEqual(self.status(), "", "anti-vacuity: main clean")
        r = self.post(tid, A_CMD)
        self._assert_a_not_refused(r.returncode, r.stderr)


class ControlsStillRefuse(ConcurrentMergeMainCwdBase):
    def test_c1_direct_commit_on_main_refused(self) -> None:
        old = self._head()
        cmd = "echo x > tracked.txt && git commit -am direct"
        tid, _ = self.pre_ok(cmd)
        (self.main / "tracked.txt").write_text("direct\n")
        git(self.main, "commit", "-q", "-am", "direct commit on main")
        self.assertNotEqual(self._head(), old, "anti-vacuity: HEAD moved")
        self.assertEqual(self.status(), "", "anti-vacuity: tree clean")
        self.assert_blocked_d(self.post(tid, cmd))

    def test_c2_reset_hard_on_main_refused(self) -> None:
        self._main_side_commit()
        old = self._head()
        cmd = "git reset --hard HEAD~1"
        tid, _ = self.pre_ok(cmd)
        git(self.main, "reset", "-q", "--hard", "HEAD~1")
        self.assertNotEqual(self._head(), old, "anti-vacuity: HEAD moved")
        self.assertEqual(self.status(), "", "anti-vacuity: tree clean")
        self.assert_blocked_d(self.post(tid, cmd))

    def test_c3_own_write_while_b_merge_completes_refused(self) -> None:
        self._side_branch_commit()
        self._main_side_commit()
        old = self._head()
        cmd = "echo mine > other.txt"
        tid, _ = self.pre_ok(cmd)
        self._b_merge(None)
        self._assert_merge_commit_landed(old)
        (self.main / "other.txt").write_text("written by session A\n")
        self.assert_blocked_d(self.post(tid, cmd), "other.txt")
        self.assertIn("other.txt", self._record(SESSION_A),
                      "A's own write must be in A's write record")


if __name__ == "__main__":
    prog = unittest.main(exit=False, verbosity=2)
    res = prog.result
    failed = len(res.failures) + len(res.errors)
    passed = res.testsRun - failed - len(res.skipped)
    print("RESULT: %d passed, %d failed" % (passed, failed))
    sys.exit(1 if failed else 0)
