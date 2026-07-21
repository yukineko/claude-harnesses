#!/usr/bin/env python3
"""Pin the git behaviour that the local gate is built on top of.

Scope, stated first so this file is not mistaken for something it is not: these
tests exercise GIT, not this repository's hooks. The verdict logic in
`.githooks/*` and `scripts/gate-bypass.py` is covered by
`scripts/test_gate_bypass.py`, written by an agent that did not implement it
(CLAUDE.md 2-(a) — the implementer does not test their own implementation).
What is pinned here is the external premise the design rests on, which the
implementer may legitimately pin because being wrong about it is not a matter of
opinion.

The premise
-----------
`git commit --no-verify` skips pre-commit, so the gate cannot refuse the bypass
at commit time. The whole arrangement — a certificate written by pre-commit, a
ledger appended by post-commit, a push refused while the ledger is non-empty —
exists because ONE hook still runs on every commit-creating path:

    plain commit         pre-commit, post-commit
    commit --no-verify   post-commit
    merge --no-ff        pre-merge-commit, post-commit
    cherry-pick          post-commit
    rebase               post-commit

Two rows are load-bearing in opposite directions:

  * post-commit must run in EVERY row. If a future git stops calling it on any
    path, ungated commits stop being recorded there and the gate silently
    develops a blind spot — silent, because nothing else would notice.
  * merge must call pre-merge-commit. Without it, this repository's own merges
    would be recorded as bypasses, and a gate that fires constantly on correct
    behaviour is a gate people learn to ignore.

If a row here goes red, the finding is not "fix the test" — it is that the
design has lost a premise and `.githooks/post-commit` needs rethinking.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HOOKS = ("pre-commit", "pre-merge-commit", "post-commit", "post-merge",
         "commit-msg")


class Repo:
    """A throwaway repo whose hooks record their own name to a log file."""

    def __init__(self) -> None:
        # The hook scripts and their log live OUTSIDE the work tree. Keeping them
        # inside it looked harmless and was not: `git add -A` committed them, so
        # `git checkout` then refused to switch branches ("local changes to
        # hooks.log would be overwritten"), the branch never changed, and the
        # cherry-pick case ended up picking a commit onto itself and reporting
        # it empty. The suite failed loudly there, but the same mistake in a
        # test that only makes negative assertions would have passed while
        # observing nothing.
        self.root = Path(tempfile.mkdtemp(prefix="hookcov-"))
        self.dir = self.root / "repo"
        self.dir.mkdir()
        self.log = self.root / "hooks.log"
        hooks = self.root / "h"
        hooks.mkdir()
        for name in HOOKS:
            p = hooks / name
            p.write_text("#!/bin/sh\necho '%s' >> '%s'\nexit 0\n" % (name, self.log))
            p.chmod(0o755)
        self.git("init", "-q", "-b", "main", ".")
        self.git("config", "core.hooksPath", str(hooks))
        self.git("config", "user.email", "t@example.com")
        self.git("config", "user.name", "t")
        self.git("config", "commit.gpgsign", "false")

    def git(self, *args: str) -> subprocess.CompletedProcess:
        return subprocess.run(
            ("git",) + args, cwd=self.dir, capture_output=True, text=True
        )

    def write(self, name: str, text: str) -> None:
        (self.dir / name).write_text(text)

    def commit(self, name: str, *extra: str) -> subprocess.CompletedProcess:
        self.write(name, name + "\n")
        self.git("add", "-A")
        return self.git("commit", "-q", "-m", name, *extra)

    def reset_log(self) -> None:
        self.log.write_text("")

    def ran(self) -> set[str]:
        if not self.log.exists():
            return set()
        return {line.strip() for line in self.log.read_text().splitlines() if line.strip()}

    def close(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)


class GitHookCoverage(unittest.TestCase):
    def setUp(self) -> None:
        self.repo = Repo()
        self.repo.commit("base", "--no-verify")
        self.repo.reset_log()

    def tearDown(self) -> None:
        self.repo.close()

    def test_plain_commit_runs_pre_commit_and_post_commit(self):
        self.repo.commit("plain")
        self.assertEqual(self.repo.ran(), {"pre-commit", "commit-msg", "post-commit"})

    def test_no_verify_skips_pre_commit(self):
        # The reason the gate cannot refuse the bypass at commit time.
        self.repo.commit("skipped", "--no-verify")
        self.assertNotIn("pre-commit", self.repo.ran())

    def test_no_verify_still_runs_post_commit(self):
        # THE load-bearing row. If this ever fails, the bypass ledger stops being
        # written and nothing downstream notices.
        self.repo.commit("skipped", "--no-verify")
        self.assertIn(
            "post-commit",
            self.repo.ran(),
            "post-commit must run even when the commit skipped verification — "
            "the bypass ledger has no other way to learn the commit happened",
        )

    def test_merge_commit_runs_pre_merge_commit_not_pre_commit(self):
        r = self.repo
        r.git("checkout", "-q", "-b", "side")
        r.commit("side", "--no-verify")
        r.git("checkout", "-q", "main")
        r.commit("mainline", "--no-verify")
        r.reset_log()
        # NOT --no-verify: that flag skips pre-merge-commit too, which would
        # have quietly disabled the very hook this test exists to observe.
        proc = r.git("merge", "--no-ff", "-m", "merge", "side")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        ran = r.ran()
        self.assertIn(
            "pre-merge-commit",
            ran,
            "a merge commit is gated by pre-merge-commit; without a hook there "
            "this repository's own merges would be recorded as bypasses",
        )
        self.assertNotIn("pre-commit", ran)
        # post-commit does NOT run for a merge. This assertion is the one that
        # caught the false premise: the design's recorder was documented as
        # covering merges and does not.
        self.assertNotIn("post-commit", ran)
        self.assertIn("post-merge", ran)

    def test_merge_no_verify_runs_only_post_merge(self):
        # The reason .githooks/post-merge has to exist: with --no-verify the
        # merge is ungated, and post-merge is the ONLY hook left to record it.
        r = self.repo
        r.git("checkout", "-q", "-b", "side")
        r.commit("side", "--no-verify")
        r.git("checkout", "-q", "main")
        r.commit("mainline", "--no-verify")
        r.reset_log()
        proc = r.git("merge", "--no-ff", "--no-verify", "-m", "merge", "side")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(
            r.ran(),
            {"post-merge"},
            "post-merge is the only recorder available for an ungated merge",
        )

    def test_cherry_pick_runs_post_commit_without_pre_commit(self):
        r = self.repo
        r.git("checkout", "-q", "-b", "src")
        r.commit("picked", "--no-verify")
        sha = r.git("rev-parse", "HEAD").stdout.strip()
        r.git("checkout", "-q", "main")
        r.reset_log()
        proc = r.git("cherry-pick", sha)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        ran = r.ran()
        # Not a false positive when the ledger records it: this content was
        # never inspected in this working tree.
        self.assertNotIn("pre-commit", ran)
        self.assertIn("post-commit", ran)


class PremiseTableIsActuallyChecked(unittest.TestCase):
    """Guard against the suite passing because it observes nothing.

    Every assertion above reads the same log file, so a Repo that silently fails
    to install its hooks would make the negative assertions pass vacuously while
    the positive ones fail. Prove the log distinguishes states.
    """

    def test_log_is_empty_before_any_commit_and_populated_after(self):
        repo = Repo()
        try:
            self.assertEqual(repo.ran(), set(), "log must start empty")
            repo.commit("first")
            self.assertTrue(repo.ran(), "log must record hooks that ran")
        finally:
            repo.close()

    def test_hooks_are_installed_where_git_looks_for_them(self):
        repo = Repo()
        try:
            configured = repo.git("config", "core.hooksPath").stdout.strip()
            self.assertEqual(configured, str(repo.root / "h"))
            for name in HOOKS:
                p = repo.root / "h" / name
                self.assertTrue(p.exists(), "%s not installed" % name)
                self.assertTrue(os.access(p, os.X_OK), "%s not executable" % name)
        finally:
            repo.close()


if __name__ == "__main__":
    unittest.main(verbosity=2)
