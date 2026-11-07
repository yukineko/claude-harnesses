#!/usr/bin/env python3
"""RED repro (backlog 78817f03): check-test-weakening does not judge a weakening of a test file
that was ADDED EARLIER ON THE SAME BRANCH (absent at merge-base(main, HEAD)).

Observed 2026-11-07: adding a skip gate (`@open_defect(...)`, a
`unittest.skipUnless(os.environ.get("AUDIT_RUN_OPEN"), ...)` wrapper) to an
existing, passing test in scripts/test_worktree_target_seed.py was not
flagged, because that file did not exist at the merge-base with main -- it
existed only in earlier commits of the same branch.

Base logic found in scripts/check-test-weakening.py (`scan`): the "before"
side is `blob_at(merge_base, old)` where merge_base = merge-base(main, HEAD),
the "after" side is the WORKTREE. For a path absent at merge-base, `old` is
None, so `before` is an empty surface:
  * `py_marker_gains` compares only test names present on BOTH sides, so no
    skip/xfail gain can ever be found; and `old_is_new` additionally filters
    every `ignore-added` finding;
  * assertion/test counts start from 0, so `after < before` is unsatisfiable.
Hence a test committed in branch commit N and weakened in commit N+1 is never
judged. The module docstring ("Within that scope it is branch-wide ... per-
commit analysis would fire on ordinary iteration ... The gap this leaves ...
is covered by a different control, the tdd F->P oracle") describes this gap as
a design decision; this RED states the observed consequence, it does not
settle that design.

Judging base the RED requires: for a file absent at merge-base, some base that
contains the branch-committed version of the test -- e.g. HEAD (per-commit:
HEAD vs worktree/staged), or every commit in merge-base..HEAD, or the file's
birth commit on the branch. The fixture has exactly one prior branch commit,
so all of these satisfy it; the RED does not choose between them.

Controls (pass today):
  * the same skip on a file that exists at merge-base blocks (ignore-added);
  * the new file's FIRST commit with an already-skipped test is allowed
    (pinned: a new skipped test hides no coverage that existed);
  * a non-weakening edit (adding a test/assertion) in commit 2 is allowed.

Repro:  python3 -m pytest --runxfail scripts/test_check_test_weakening_branch_new_file.py
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"

PASSING = """\
import unittest


class T(unittest.TestCase):
    def test_a(self):
        self.assertEqual(1 + 1, 2)
        self.assertTrue(True)

    def test_b(self):
        self.assertIn("x", "xyz")


if __name__ == "__main__":
    unittest.main()
"""

# test_a gains a skip; nothing else changes.
SKIPPED_A = PASSING.replace(
    "    def test_a(self):\n",
    "    @unittest.skip(\"open defect\")\n    def test_a(self):\n",
)

# test_a loses one of its two assertions; nothing else changes.
ASSERT_REMOVED_A = PASSING.replace("        self.assertTrue(True)\n", "")

# A strengthening edit: one more assertion in test_b, plus a new test.
STRENGTHENED = PASSING.replace(
    "        self.assertIn(\"x\", \"xyz\")\n",
    "        self.assertIn(\"x\", \"xyz\")\n        self.assertNotIn(\"q\", \"xyz\")\n\n"
    "    def test_c(self):\n        self.assertEqual(3, 3)\n",
)

EXISTING_REL = "scripts/test_existing.py"
NEW_REL = "scripts/test_branch_new.py"


class BranchNewFileWeakening(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twbranchnew.")
        self.addCleanup(td.cleanup)
        self.tmp = Path(os.path.realpath(td.name))
        home = self.tmp / "home"
        home.mkdir()
        self.env = {
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
            "HOME": str(home), "TMPDIR": str(self.tmp), "LC_ALL": "C",
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        }
        self.repo = self.tmp / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        self.git("init", "-q", "-b", "main")
        self.write(EXISTING_REL, PASSING)
        self.commit("C0 on main")
        self.main_sha = self.git("rev-parse", "HEAD").strip()
        self.git("checkout", "-q", "-b", "feat")

    # -- helpers ---------------------------------------------------------

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def write(self, rel: str, src: str) -> None:
        (self.repo / rel).write_text(src)

    def commit(self, msg: str) -> None:
        self.git("add", "-A")
        self.git("-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
                 "commit", "-q", "-m", msg)

    def scan(self) -> tuple[int, dict, str]:
        """Run the gate the way .githooks/commit-msg does for the staged commit."""
        self.git("add", "-A")
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("commit 2\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg), "--json"],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        try:
            data = json.loads(r.stdout)
        except ValueError:
            data = {}
        return r.returncode, data, f"rc={r.returncode} stdout={r.stdout!r} stderr={r.stderr!r}"

    def kinds(self, data: dict, rel: str) -> set:
        return {f["kind"] for f in data.get("findings", []) if f["path"] == rel}

    def branch_commit_1(self, src: str) -> None:
        """Branch commit 1: add NEW_REL (absent on main), then anti-vacuity."""
        self.write(NEW_REL, src)
        self.commit("commit 1: add test file on branch")
        mb = self.git("merge-base", "main", "HEAD").strip()
        self.assertEqual(mb, self.main_sha, "anti-vacuity: fork point is main's C0")
        self.assertEqual(self.git("ls-tree", "--name-only", mb, "--", NEW_REL), "",
                         "anti-vacuity: the file is absent at the merge-base")
        self.assertEqual(self.git("ls-tree", "--name-only", "HEAD", "--", NEW_REL).strip(),
                         NEW_REL, "anti-vacuity: the file is committed at HEAD")

    # -- RED -------------------------------------------------------------

    def test_skip_added_to_test_committed_earlier_on_branch_blocks(self) -> None:
        self.branch_commit_1(PASSING)
        self.write(NEW_REL, SKIPPED_A)
        self.assertIn(NEW_REL, self.git("diff", "--name-only", "HEAD"),
                      "anti-vacuity: commit 2 changes the file")
        rc, data, out = self.scan()
        self.assertEqual(rc, 1, "commit 2 adds a skip to test_a, which passed in "
                         "branch commit 1, yet the gate did not block: " + out)
        self.assertIn("ignore-added", self.kinds(data, NEW_REL), out)

    @unittest.expectedFailure  # backlog 78817f03: open defect
    def test_assertion_removed_from_test_committed_earlier_on_branch_blocks(self) -> None:
        self.branch_commit_1(PASSING)
        self.write(NEW_REL, ASSERT_REMOVED_A)
        rc, data, out = self.scan()
        self.assertEqual(rc, 1, "commit 2 removes an assertion from test_a, which "
                         "branch commit 1 had, yet the gate did not block: " + out)
        self.assertIn("assertion-removed", self.kinds(data, NEW_REL), out)

    # -- controls (pass today) --------------------------------------------

    def test_control_skip_added_to_file_existing_on_main_blocks(self) -> None:
        self.write(EXISTING_REL, SKIPPED_A)
        rc, data, out = self.scan()
        self.assertEqual(rc, 1, out)
        self.assertIn("ignore-added", self.kinds(data, EXISTING_REL), out)

    def test_control_assertion_removed_from_file_existing_on_main_blocks(self) -> None:
        self.write(EXISTING_REL, ASSERT_REMOVED_A)
        rc, data, out = self.scan()
        self.assertEqual(rc, 1, out)
        self.assertIn("assertion-removed", self.kinds(data, EXISTING_REL), out)

    def test_pin_first_commit_of_new_file_with_skipped_test_is_allowed(self) -> None:
        # Commit 1 alone (staged, not yet committed): a brand-new file whose
        # test is already skipped. Pinned as it behaves at HEAD.
        self.write(NEW_REL, SKIPPED_A)
        rc, data, out = self.scan()
        self.assertEqual(rc, 0, out)
        self.assertEqual(self.kinds(data, NEW_REL), set(), out)

    def test_control_non_weakening_edit_in_commit_2_is_allowed(self) -> None:
        self.branch_commit_1(PASSING)
        self.write(NEW_REL, STRENGTHENED)
        rc, data, out = self.scan()
        self.assertEqual(rc, 0, out)
        self.assertEqual(self.kinds(data, NEW_REL), set(), out)


if __name__ == "__main__":
    unittest.main()
