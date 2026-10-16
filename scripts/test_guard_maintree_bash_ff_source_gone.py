#!/usr/bin/env python3
"""Fast-forward onto main whose source branch is already gone at PostToolUse.

Observed 2026-10-16 (several times, transcript c5b9c02b): the guard refused
real fast-forwards with "HEAD <a> -> <b> (not a merge / fast-forward)".
integration_head() in scripts/guard-maintree-bash.py accepts a moved HEAD only
if it is a merge commit or is contained in a ref other than main's branch. A
fast-forward whose source branch is deleted before Post (in the same Bash call,
or by another session during the call) fails both checks.

User ruling 2026-10-16: when no other ref contains the new HEAD, a reflog entry
for main of the form "merge ...: Fast-forward" or "merge ..." is integration
evidence. A reflog "commit:" entry (a direct commit) is still refused. A reflog
that cannot be read is undetermined and is refused (CLAUDE.md 3).

Fixed (backlog 86e21645): test_a / test_b were RED under @unittest.expectedFailure
and are now plain tests:

    python3 -m pytest scripts/test_guard_maintree_bash_ff_source_gone.py

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_guard_maintree_bash_observe import ObserveGuardBase, _clean_env, git  # noqa: E402


class FfSourceGoneBase(ObserveGuardBase):
    def _head(self) -> str:
        return git(self.main, "rev-parse", "HEAD").stdout.strip()

    def _wt_commit(self) -> str:
        (self.wt / "new.txt").write_text("n\n")
        git(self.wt, "add", "new.txt")
        git(self.wt, "commit", "-q", "-m", "wt non-merge commit")
        return git(self.wt, "rev-parse", "HEAD").stdout.strip()

    def _ff_and_delete_source(self) -> None:
        git(self.main, "merge", "-q", "--ff-only", "wtbranch")
        git(self.main, "worktree", "remove", str(self.wt))
        git(self.main, "branch", "-q", "-d", "wtbranch")

    def _only_main_contains_head(self) -> None:
        refs = git(self.main, "for-each-ref", "--contains", "HEAD",
                   "--format=%(refname)").stdout.split()
        self.assertEqual(refs, ["refs/heads/main"],
                         "anti-vacuity: no ref other than main contains the new HEAD")
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertEqual(len(parents), 2, "anti-vacuity: the new HEAD is not a merge commit")

    def _assert_ff_shape(self, old: str, tip: str) -> None:
        self.assertEqual(self._head(), tip, "anti-vacuity: main moved to the source tip")
        self.assertNotEqual(old, tip)
        r = git(self.main, "merge-base", "--is-ancestor", old, tip, check=False)
        self.assertEqual(r.returncode, 0, "anti-vacuity: old HEAD is an ancestor")
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean")
        self._only_main_contains_head()

    def _main_reflog_top(self) -> str:
        return git(self.main, "reflog", "-n1", "--format=%gs", "refs/heads/main").stdout.strip()


class FfSourceGoneAllowed(FfSourceGoneBase):
    """Allowed because the guard reads main's reflog as integration evidence."""

    def test_a_ff_and_branch_delete_in_same_call_is_allowed(self) -> None:
        tip = self._wt_commit()
        old = self._head()
        cmd = ("git merge --ff-only wtbranch && git worktree remove wt"
               " && git branch -d wtbranch")
        tid, _ = self.pre_ok(cmd)
        self._ff_and_delete_source()
        self._assert_ff_shape(old, tip)
        self.assertRegex(self._main_reflog_top(), r"^merge wtbranch: Fast-forward$")
        self.assert_allowed(self.post(tid, cmd))

    def test_b_other_session_ff_and_branch_delete_during_unrelated_call_is_allowed(self) -> None:
        tip = self._wt_commit()
        old = self._head()
        tid, _ = self.pre_ok("git status")
        # A DIFFERENT process (another session) integrates and deletes its branch
        # between this call's Pre and Post; this call only ran `git status`.
        script = (
            f"git -C '{self.main}' merge -q --ff-only wtbranch && "
            f"git -C '{self.main}' worktree remove '{self.wt}' && "
            f"git -C '{self.main}' branch -q -d wtbranch")
        subprocess.run(["sh", "-c", script], check=True, env=_clean_env(),
                       capture_output=True, text=True)
        self._assert_ff_shape(old, tip)
        self.assertRegex(self._main_reflog_top(), r"^merge wtbranch: Fast-forward$")
        self.assert_allowed(self.post(tid, "git status"))


class StillRefused(FfSourceGoneBase):
    """Controls: GREEN now and must stay GREEN after the fix."""

    def test_c_direct_commit_on_main_refused(self) -> None:
        old = self._head()
        tid, _ = self.pre_ok("echo x > tracked.txt && git commit -am direct")
        (self.main / "tracked.txt").write_text("direct\n")
        git(self.main, "commit", "-q", "-am", "direct commit on main")
        self.assertNotEqual(old, self._head())
        self.assertEqual(self.status(), "")
        self._only_main_contains_head()
        self.assertTrue(self._main_reflog_top().startswith("commit: "))
        self.assert_blocked_d(self.post(tid))

    def test_c_reset_to_unrelated_commit_refused(self) -> None:
        git(self.main, "checkout", "-q", "--orphan", "unrelated")
        git(self.main, "commit", "-q", "--allow-empty", "-m", "unrelated root")
        unrelated = self._head()
        git(self.main, "checkout", "-q", "-f", "main")
        git(self.main, "branch", "-q", "-D", "unrelated")
        git(self.main, "clean", "-fdq")
        tid, _ = self.pre_ok(f"git reset --hard {unrelated}")
        git(self.main, "reset", "-q", "--hard", unrelated)
        self.assertEqual(self._head(), unrelated)
        r = git(self.main, "merge-base", "--is-ancestor", "HEAD@{1}", "HEAD", check=False)
        self.assertNotEqual(r.returncode, 0, "anti-vacuity: not a fast-forward")
        self.assertEqual(self.status(), "")
        self.assertTrue(self._main_reflog_top().startswith("reset: "))
        r = self.post(tid)
        self.assertEqual(r.returncode, 2, r.stderr)
        self.assertIn("not a merge / fast-forward", r.stderr)

    def test_c_direct_commit_whose_subject_mimics_a_ff_reflog_refused(self) -> None:
        # The subject imitates a fast-forward reflog line; the reflog entry
        # itself still starts with "commit:". Matching "Fast-forward" or
        # "merge " anywhere in the line would wrongly allow this.
        old = self._head()
        subject = "merge wtbranch: Fast-forward"
        tid, _ = self.pre_ok(f"git commit -am '{subject}'")
        (self.main / "tracked.txt").write_text("forged\n")
        git(self.main, "commit", "-q", "-am", subject)
        self.assertNotEqual(old, self._head())
        self.assertEqual(self.status(), "")
        self._only_main_contains_head()
        self.assertEqual(self._main_reflog_top(), "commit: " + subject)
        self.assert_blocked_d(self.post(tid))

    def test_c_ff_then_direct_commit_in_same_call_refused(self) -> None:
        # A real fast-forward entry exists in the reflog for this call, but the
        # final HEAD is a direct commit on top of it.
        self._wt_commit()
        tid, _ = self.pre_ok("git merge --ff-only wtbranch && git branch -d ... && git commit")
        self._ff_and_delete_source()
        ff_entry = self._main_reflog_top()
        self.assertRegex(ff_entry, r"^merge wtbranch: Fast-forward$")
        (self.main / "tracked.txt").write_text("after ff\n")
        git(self.main, "commit", "-q", "-am", "direct commit after the ff")
        self.assertEqual(self.status(), "")
        self._only_main_contains_head()
        self.assertTrue(self._main_reflog_top().startswith("commit: "))
        self.assert_blocked_d(self.post(tid))


class ReflogUnreadableRefused(FfSourceGoneBase):
    """(d) Without a readable reflog there is no integration evidence: refuse."""

    def _ff_without_reflog(self, disable_logging: bool) -> tuple[str, str]:
        tip = self._wt_commit()
        old = self._head()
        if disable_logging:
            git(self.main, "config", "core.logAllRefUpdates", "false")
        for rel in ("logs/refs/heads/main", "logs/HEAD"):
            p = self.main / ".git" / rel
            if p.exists():
                p.unlink()
        return old, tip

    def _assert_no_reflog(self) -> None:
        r = git(self.main, "reflog", "-n1", "--format=%gs", "refs/heads/main", check=False)
        self.assertEqual(r.stdout.strip(), "", f"anti-vacuity: main has no reflog entry: {r!r}")

    def test_d_ff_source_gone_with_logallrefupdates_false_refused(self) -> None:
        old, tip = self._ff_without_reflog(disable_logging=True)
        cmd = "git merge --ff-only wtbranch && git branch -d wtbranch"
        tid, _ = self.pre_ok(cmd)
        self._ff_and_delete_source()
        self._assert_ff_shape(old, tip)
        self._assert_no_reflog()
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 2, f"undetermined must refuse: {r.stderr!r}")
        self.assertTrue(r.stderr.strip())

    def test_d_ff_source_gone_with_reflog_file_missing_refused(self) -> None:
        old, tip = self._ff_without_reflog(disable_logging=False)
        cmd = "git merge --ff-only wtbranch && git branch -d wtbranch"
        tid, _ = self.pre_ok(cmd)
        self._ff_and_delete_source()
        # The ff recreated the log (logAllRefUpdates defaults to true in a
        # non-bare repo); remove it again so the reflog is missing at Post.
        for rel in ("logs/refs/heads/main", "logs/HEAD"):
            p = self.main / ".git" / rel
            if p.exists():
                p.unlink()
        self._assert_ff_shape(old, tip)
        self._assert_no_reflog()
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 2, f"undetermined must refuse: {r.stderr!r}")
        self.assertTrue(r.stderr.strip())


if __name__ == "__main__":
    unittest.main(verbosity=2)
