#!/usr/bin/env python3
"""The main-tree guard judges a moved HEAD from main's OWN state only.

User ruling 2026-11-08: gates judge only their own worktree and read no other
branch's refs. At PostToolUse of a Bash call whose cwd is main, a HEAD move
with an otherwise unchanged tree may be allowed only when main's own branch
reflog explains it: a chain of entries whose messages start with "merge "
(git merge, incl. "merge x: Fast-forward") or "commit (merge): " (a commit
concluding a merge), each merge-commit step having the previous HEAD as its
first parent and each fast-forward step descending from the previous HEAD.
No ref enumeration of other branches (for-each-ref --contains etc.) may run.

Consequences pinned here:
  1. non-ff merge on main            -> allowed (control)
  2. ff merge on main                -> allowed (control)
  3. direct non-merge commit on main -> refused (control)
  4. the guard runs no for-each-ref / --contains / other-branch ref query
     at Pre or Post (observed through a logging git shim on PATH)
  5. newly refused: (a) `git reset --hard wtbranch` (ff-shaped, reflog
     "reset: ..."); (b) `git reset --hard <merge commit made on another
     branch, descending from main's HEAD>`.

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import os
import shutil
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_guard_maintree_bash_observe import ObserveGuardBase, git  # noqa: E402

FORBIDDEN_TOKENS_EXACT = ("for-each-ref", "show-ref", "--all")
FORBIDDEN_TOKEN_PREFIXES = ("--contains", "--merged", "--no-merged", "--branches")
FORBIDDEN_SUBSTRINGS = ("refs/heads/wtbranch", "wtbranch")


class OwnStateBase(ObserveGuardBase):
    def _head(self, repo: Path | None = None) -> str:
        return git(repo or self.main, "rev-parse", "HEAD").stdout.strip()

    def _wt_commit(self, name: str = "new.txt") -> str:
        (self.wt / name).write_text(name + "\n")
        git(self.wt, "add", name)
        git(self.wt, "commit", "-q", "-m", f"wt commit {name}")
        return self._head(self.wt)

    def _parents(self, rev: str) -> list[str]:
        return git(self.main, "rev-list", "--parents", "-n1", rev).stdout.split()[1:]

    def _main_reflog_top(self) -> str:
        return git(self.main, "reflog", "-n1", "--format=%gs",
                   "refs/heads/main").stdout.strip()

    def _is_ancestor(self, a: str, b: str) -> bool:
        return git(self.main, "merge-base", "--is-ancestor", a, b,
                   check=False).returncode == 0

    # -- scenarios: each performs the "command" between Pre and Post --------
    def scenario_no_ff_merge(self, **kw):
        self._wt_commit()
        old = self._head()
        cmd = "git merge --no-ff -m 'merge wt' wtbranch"
        tid, _ = self.pre_ok(cmd, **kw)
        git(self.main, "merge", "-q", "--no-ff", "-m", "merge wt", "wtbranch")
        new = self._head()
        self.assertNotEqual(old, new, "anti-vacuity: HEAD moved")
        parents = self._parents(new)
        self.assertEqual(len(parents), 2, "anti-vacuity: new HEAD is a merge commit")
        self.assertEqual(parents[0], old, "anti-vacuity: first parent is old HEAD")
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean")
        self.assertTrue(self._main_reflog_top().startswith("merge wtbranch: "),
                        self._main_reflog_top())
        return tid, cmd

    def scenario_ff_merge(self, **kw):
        tip = self._wt_commit()
        old = self._head()
        cmd = "git merge --ff-only wtbranch"
        tid, _ = self.pre_ok(cmd, **kw)
        git(self.main, "merge", "-q", "--ff-only", "wtbranch")
        self.assertEqual(self._head(), tip, "anti-vacuity: main fast-forwarded")
        self.assertNotEqual(old, tip)
        self.assertTrue(self._is_ancestor(old, tip))
        self.assertEqual(len(self._parents(tip)), 1, "anti-vacuity: not a merge commit")
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean")
        self.assertEqual(self._main_reflog_top(), "merge wtbranch: Fast-forward")
        return tid, cmd

    def scenario_direct_commit(self, **kw):
        old = self._head()
        cmd = "echo x > tracked.txt && git commit -am direct"
        tid, _ = self.pre_ok(cmd, **kw)
        (self.main / "tracked.txt").write_text("direct\n")
        git(self.main, "commit", "-q", "-am", "direct commit on main")
        new = self._head()
        self.assertNotEqual(old, new, "anti-vacuity: HEAD moved")
        self.assertEqual(self._parents(new), [old], "anti-vacuity: plain child commit")
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean")
        self.assertTrue(self._main_reflog_top().startswith("commit: "),
                        self._main_reflog_top())
        return tid, cmd


    def scenario_ff_pull(self, **kw):
        tip = self._wt_commit()
        old = self._head()
        cmd = "git pull --ff-only . wtbranch"
        tid, _ = self.pre_ok(cmd, **kw)
        git(self.main, "pull", "-q", "--ff-only", ".", "wtbranch")
        self.assertEqual(self._head(), tip, "anti-vacuity: main fast-forwarded")
        self.assertNotEqual(old, tip, "anti-vacuity: HEAD moved")
        self.assertTrue(self._is_ancestor(old, tip), "anti-vacuity: old is ancestor")
        self.assertEqual(len(self._parents(tip)), 1, "anti-vacuity: not a merge commit")
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean")
        self.assertRegex(self._main_reflog_top(), r"^pull.*: Fast-forward$")
        return tid, cmd


class MergesOnMainAllowed(OwnStateBase):
    def test_1_no_ff_merge_commit_on_main_allowed(self) -> None:
        tid, cmd = self.scenario_no_ff_merge()
        self.assert_allowed(self.post(tid, cmd))

    def test_2_fast_forward_merge_on_main_allowed(self) -> None:
        tid, cmd = self.scenario_ff_merge()
        self.assert_allowed(self.post(tid, cmd))


class DirectCommitRefused(OwnStateBase):
    def test_3_direct_non_merge_commit_on_main_refused(self) -> None:
        tid, cmd = self.scenario_direct_commit()
        self.assert_blocked_d(self.post(tid, cmd))


class NoOtherBranchRefQueries(OwnStateBase):
    """Observe every git argv the guard runs through a logging shim on PATH."""

    def setUp(self) -> None:
        super().setUp()
        real_git = shutil.which("git")
        self.assertTrue(real_git, "git must be on PATH")
        d = self.tmp / "logshim"
        d.mkdir()
        self.log = self.tmp / "git-argv.log"
        g = d / "git"
        # one arg per line, each invocation terminated by a separator line
        g.write_text(
            "#!/bin/sh\n"
            f"printf '%s\\n' \"$@\" >> '{self.log}'\n"
            f"printf '%s\\n' '<<END>>' >> '{self.log}'\n"
            f"exec '{real_git}' \"$@\"\n")
        g.chmod(0o755)
        self.env = {"PATH": f"{d}{os.pathsep}{os.environ.get('PATH', '')}"}

    def _invocations(self) -> list[list[str]]:
        if not self.log.exists():
            return []
        calls, cur = [], []
        for line in self.log.read_text().splitlines():
            if line == "<<END>>":
                calls.append(cur)
                cur = []
            else:
                cur.append(line)
        return calls

    def _reset_log(self) -> None:
        if self.log.exists():
            self.log.unlink()

    def _offending(self, calls: list[list[str]]) -> list[list[str]]:
        bad = []
        for argv in calls:
            for tok in argv:
                if (tok in FORBIDDEN_TOKENS_EXACT
                        or tok.startswith(FORBIDDEN_TOKEN_PREFIXES)
                        or any(s in tok for s in FORBIDDEN_SUBSTRINGS)):
                    bad.append(argv)
                    break
        return bad

    def _check(self, phase: str) -> None:
        calls = self._invocations()
        self.assertTrue(calls, f"anti-vacuity: the shim logged no git call at {phase}"
                               " (the guard did not use git from PATH)")
        bad = self._offending(calls)
        self.assertEqual(bad, [], f"{phase}: the guard queried other branches' refs: {bad}")

    def _run(self, scenario, expect_rc: int) -> None:
        # Pre runs through the shim (scenario passes env_override to pre_ok);
        # the test's own git calls do not use the shim (they use the base git()).
        self._reset_log()
        tid, cmd = scenario(env_override=self.env)
        self._check("Pre")
        self._reset_log()
        r = self.post(tid, cmd, env_override=self.env)
        self.assertEqual(r.returncode, expect_rc, f"rc; stderr={r.stderr!r}")
        self._check("Post")

    def test_4a_ff_case_no_ref_enumeration(self) -> None:
        self._run(self.scenario_ff_merge, 0)

    def test_4b_merge_commit_case_no_ref_enumeration(self) -> None:
        self._run(self.scenario_no_ff_merge, 0)

    def test_4c_refused_direct_commit_case_no_ref_enumeration(self) -> None:
        self._run(self.scenario_direct_commit, 2)

    def test_6_ff_pull_on_main_allowed_no_ref_enumeration(self) -> None:
        self._run(self.scenario_ff_pull, 0)


class PullAndMergeTipShapes(OwnStateBase):
    def test_6_ff_pull_on_main_allowed(self) -> None:
        tid, cmd = self.scenario_ff_pull()
        self.assert_allowed(self.post(tid, cmd))

    def test_7_ff_to_a_merge_commit_tip_allowed(self) -> None:
        old = self._head()
        git(self.main, "branch", "side", old)
        side_wt = self.tmp / "side"
        git(self.main, "worktree", "add", "-q", str(side_wt), "side")
        (side_wt / "side.txt").write_text("s\n")
        git(side_wt, "add", "side.txt")
        git(side_wt, "commit", "-q", "-m", "side commit")
        self._wt_commit()
        git(self.wt, "merge", "-q", "--no-ff", "-m", "merge side into wt", "side")
        tip = self._head(self.wt)
        cmd = "git merge --ff-only wtbranch"
        tid, _ = self.pre_ok(cmd)
        git(self.main, "merge", "-q", "--ff-only", "wtbranch")
        self.assertEqual(self._head(), tip, "anti-vacuity: main fast-forwarded to tip")
        self.assertTrue(self._is_ancestor(old, tip))
        parents = self._parents(tip)
        self.assertEqual(len(parents), 2, "anti-vacuity: the tip is a merge commit")
        self.assertNotEqual(parents[0], old,
                            "anti-vacuity: first parent is NOT main's old HEAD")
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean")
        self.assertEqual(self._main_reflog_top(), "merge wtbranch: Fast-forward")
        self.assert_allowed(self.post(tid, cmd))

    def test_8_pull_rebase_refused(self) -> None:
        self._wt_commit("wt_side.txt")
        (self.main / "tracked.txt").write_text("local on main\n")
        git(self.main, "commit", "-q", "-am", "local commit on main")
        old = self._head()
        wt_tip = self._head(self.wt)
        self.assertFalse(self._is_ancestor(old, wt_tip), "anti-vacuity: diverged")
        self.assertFalse(self._is_ancestor(wt_tip, old), "anti-vacuity: diverged")
        cmd = "git pull --rebase . wtbranch"
        tid, _ = self.pre_ok(cmd)
        git(self.main, "pull", "-q", "--rebase", ".", "wtbranch")
        new = self._head()
        self.assertNotEqual(old, new, "anti-vacuity: HEAD moved")
        self.assertEqual(self._parents(new), [wt_tip],
                         "anti-vacuity: local commit was replayed onto wtbranch")
        self.assertFalse(self._is_ancestor(old, new), "anti-vacuity: not a ff")
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean")
        self.assertRegex(self._main_reflog_top(), r"^pull .*--rebase.*\(finish\)")
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 2, f"expected refusal; stderr={r.stderr!r}")
        self.assertTrue(r.stderr.strip(), "a refusal must carry a reason")


class ResetMovesRefused(OwnStateBase):
    """HEAD moves not explained by main's own merge reflog entries."""

    def test_5a_reset_hard_to_wtbranch_tip_refused(self) -> None:
        tip = self._wt_commit()
        old = self._head()
        cmd = "git reset --hard wtbranch"
        tid, _ = self.pre_ok(cmd)
        git(self.main, "reset", "-q", "--hard", "wtbranch")
        self.assertEqual(self._head(), tip, "anti-vacuity: main moved to wtbranch tip")
        self.assertTrue(self._is_ancestor(old, tip), "anti-vacuity: ff-shaped move")
        self.assertEqual(len(self._parents(tip)), 1, "anti-vacuity: not a merge commit")
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean")
        refs = git(self.main, "for-each-ref", "--contains", tip,
                   "--format=%(refname)").stdout.split()
        self.assertIn("refs/heads/wtbranch", refs,
                      "anti-vacuity: the tip lives on another branch")
        self.assertTrue(self._main_reflog_top().startswith("reset: "),
                        self._main_reflog_top())
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 2, f"expected refusal; stderr={r.stderr!r}")
        self.assertTrue(r.stderr.strip(), "a refusal must carry a reason")

    def test_5b_reset_hard_to_merge_commit_made_on_other_branch_refused(self) -> None:
        old = self._head()
        # side branch off main's HEAD, merged into wtbranch with a merge commit
        git(self.main, "branch", "side", old)
        side_wt = self.tmp / "side"
        git(self.main, "worktree", "add", "-q", str(side_wt), "side")
        (side_wt / "side.txt").write_text("s\n")
        git(side_wt, "add", "side.txt")
        git(side_wt, "commit", "-q", "-m", "side commit")
        self._wt_commit()
        git(self.wt, "merge", "-q", "--no-ff", "-m", "merge side into wt", "side")
        mc = self._head(self.wt)
        self.assertEqual(len(self._parents(mc)), 2, "anti-vacuity: mc is a merge commit")
        self.assertTrue(self._is_ancestor(old, mc), "anti-vacuity: main HEAD is ancestor")
        cmd = f"git reset --hard {mc}"
        tid, _ = self.pre_ok(cmd)
        git(self.main, "reset", "-q", "--hard", mc)
        self.assertEqual(self._head(), mc, "anti-vacuity: main moved to mc")
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean")
        self.assertTrue(self._main_reflog_top().startswith("reset: "),
                        self._main_reflog_top())
        r = self.post(tid, cmd)
        self.assertEqual(r.returncode, 2, f"expected refusal; stderr={r.stderr!r}")
        self.assertTrue(r.stderr.strip(), "a refusal must carry a reason")


if __name__ == "__main__":
    unittest.main(verbosity=2)
