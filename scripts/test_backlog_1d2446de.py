#!/usr/bin/env python3
"""Repro test for backlog 1d2446de: a fast-forward / squash onto an already-GATED
merge commit is recorded in the bypass ledger as ungated.

`.git/gate-bypassed` is written by `.githooks/post-commit`, which
`.githooks/post-merge` execs for merges. It must record only commits that were
created WITHOUT passing the gate. A fast-forward and a `merge --squash` create no
commit at all, so they must record nothing. Observed instead: when the branch
fast-forwards onto a merge commit M that was itself made through the gate
(`merge --no-ff`, which runs pre-merge-commit green), M is appended to the
ledger; two such fast-forwards append M twice. `merge --squash` while HEAD is a
gated merge appends HEAD.

Every test runs in a THROWAWAY repository built by `GateHarness` from
`scripts/test_gate_bypass.py` (the real `.githooks` copied in, scanners stubbed
green). Nothing touches the real repository's `.git`.

The controls (`Controls`) pin that recording still happens where it must, so a
"fix" that simply stops post-merge from recording is caught.

    python3 -m unittest scripts/test_backlog_1d2446de.py -v
"""

import os
import shutil
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))

from test_gate_bypass import GateTestCase  # noqa: E402


def _gated_merge(tc, h):
    """Build main = M, a merge commit made THROUGH the gate.

    Returns (M, first_parent_of_M). Asserts the ledger is empty right after the
    gated merge (control: M itself is not a bypass)."""
    h.git("checkout", "-q", "-b", "feat")
    h.write("b.txt", "b\n")
    h.git("add", "-A")
    h.commit("feature")
    h.git("checkout", "-q", "main")
    h.write("m.txt", "m\n")
    h.git("add", "-A")
    h.commit("main change")
    tc.assertLedgerEmpty(h, "before the merge")

    h.clear_log()
    h.git("merge", "--no-ff", "-m", "gated merge feat", "feat")
    tc.assertTrue(h.ran(), "precondition: pre-merge-commit ran the gates")
    tc.assertLedgerEmpty(h, "right after a GATED merge --no-ff (control)")

    m = h.git("rev-parse", "HEAD").stdout.strip()
    parents = h.git("rev-parse", "HEAD^@").stdout.split()
    tc.assertEqual(len(parents), 2, "precondition: M is a real merge commit")
    return m, parents[0]


class FastForwardOntoGatedMerge(GateTestCase):
    def test_ff_only_onto_gated_merge_records_nothing(self):
        h = self.harness()
        m, base = _gated_merge(self, h)

        h.git("checkout", "-q", "-b", "catchup", base)
        h.git("merge", "--ff-only", "main")
        self.assertEqual(h.git("rev-parse", "HEAD").stdout.strip(), m)
        self.assertLedgerEmpty(
            h,
            "a fast-forward creates no commit; M was gated, so nothing may be "
            "recorded",
        )

    def test_plain_merge_that_fast_forwards_onto_gated_merge_records_nothing(self):
        h = self.harness()
        m, base = _gated_merge(self, h)

        h.git("checkout", "-q", "-b", "catchup", base)
        h.git("merge", "main")
        self.assertEqual(
            h.git("rev-parse", "HEAD").stdout.strip(),
            m,
            "precondition: plain `git merge` fast-forwarded",
        )
        self.assertLedgerEmpty(
            h, "a plain merge that fast-forwards onto gated M records nothing"
        )

    def test_two_fast_forwards_onto_same_gated_merge_record_nothing(self):
        h = self.harness()
        m, base = _gated_merge(self, h)

        # Branch 1 in the main checkout.
        h.git("checkout", "-q", "-b", "one", base)
        h.git("merge", "--ff-only", "main")
        self.assertEqual(h.git("rev-parse", "HEAD").stdout.strip(), m)

        # Branch 2 in a linked worktree (shares the ledger, own git dir).
        other = h.root.parent / (h.root.name + "-wt")
        self.addCleanup(shutil.rmtree, str(other), True)
        h.git("worktree", "add", "-q", "-b", "two", str(other), base)
        h.git("merge", "--ff-only", "main", cwd=other)
        self.assertEqual(h.git("rev-parse", "HEAD", cwd=other).stdout.strip(), m)

        lines = h.ledger_lines()
        self.assertNotEqual(
            len(lines), 2, "M recorded once per fast-forward:\n" + "\n".join(lines)
        )
        self.assertLedgerEmpty(
            h, "two fast-forwards onto the same gated M create no commit"
        )


class SquashOntoGatedMerge(GateTestCase):
    def test_squash_while_head_is_gated_merge_records_nothing(self):
        h = self.harness()
        m, _ = _gated_merge(self, h)

        h.git("checkout", "-q", "-b", "y", m)
        h.write("y.txt", "y\n")
        h.git("add", "-A")
        h.commit("y change")
        h.git("checkout", "-q", "main")
        self.assertLedgerEmpty(h, "before the squash")

        h.git("merge", "--squash", "y")
        self.assertEqual(
            h.git("rev-parse", "HEAD").stdout.strip(),
            m,
            "precondition: --squash created no commit",
        )
        self.assertLedgerEmpty(
            h, "merge --squash creates no commit; HEAD (gated M) must not be recorded"
        )


class Controls(GateTestCase):
    def _ungated_merge(self, h):
        h.git("checkout", "-q", "-b", "feat")
        h.write("b.txt", "b\n")
        h.git("add", "-A")
        h.commit("feature")
        h.git("checkout", "-q", "main")
        h.write("m.txt", "m\n")
        h.git("add", "-A")
        h.commit("main change")
        self.assertLedgerEmpty(h, "before the merge")
        base = h.git("rev-parse", "HEAD").stdout.strip()

        h.clear_log()
        h.git("merge", "--no-ff", "--no-verify", "-m", "ungated merge", "feat")
        self.assertEqual(h.ran(), [], "precondition: no gate ran")
        m = h.git("rev-parse", "HEAD").stdout.strip()
        return m, base

    def test_ungated_merge_is_recorded_once(self):
        h = self.harness()
        m, _ = self._ungated_merge(h)
        lines = h.ledger_lines()
        self.assertLedgerHas(h, 1, "the ungated merge must be recorded")
        self.assertTrue(lines[0].startswith(m), "the entry is the merge itself")

    def test_ff_onto_ungated_merge_does_not_add_second_entry(self):
        """Half control, half defect. The first assertion (the ungated merge IS
        recorded, once) guards against a fix that stops recording. The second
        is the same defect from the other side: observed RED on the code at the
        time this test was written — the fast-forward appended the ungated
        merge a second time (2 entries for one commit)."""
        h = self.harness()
        m, base = self._ungated_merge(h)
        self.assertLedgerHas(h, 1, "after the ungated merge")

        h.git("checkout", "-q", "-b", "catchup", base)
        h.git("merge", "--ff-only", "main")
        self.assertEqual(h.git("rev-parse", "HEAD").stdout.strip(), m)
        self.assertLedgerHas(
            h, 1, "a fast-forward onto the ungated merge creates no new commit"
        )
        self.assertTrue(h.ledger_lines()[0].startswith(m))

    def test_commit_no_verify_is_recorded(self):
        h = self.harness()
        h.write("x.txt", "x\n")
        h.git("add", "-A")
        proc = h.commit("ungated", "--no-verify")
        self.assertEqual(proc.returncode, 0)
        self.assertLedgerHas(h, 1, "commit --no-verify must still be recorded")


class UngatedMergeCannotDisguiseItselfAsFastForward(GateTestCase):
    """An ungated `merge --no-ff --no-verify` IS a new commit that skipped the
    gate. Whatever lets the hook tell "this merge created a commit" apart from
    "this was a fast-forward" must not be steerable by caller-controlled text
    (reflog action, branch name, message) and must not depend on reflogs
    existing. Each case must still record exactly 1 entry: the new merge."""

    def _prepare(self, h, branch="feat"):
        h.git("checkout", "-q", "-b", branch)
        h.write("b.txt", "b\n")
        h.git("add", "-A")
        h.commit("feature")
        h.git("checkout", "-q", "main")
        h.write("m.txt", "m\n")
        h.git("add", "-A")
        h.commit("main change")
        self.assertLedgerEmpty(h, "before the merge")
        h.clear_log()

    def _assert_one_entry_for_new_merge(self, h):
        self.assertEqual(h.ran(), [], "precondition: no gate ran")
        head = h.git("rev-parse", "HEAD").stdout.strip()
        self.assertEqual(
            len(h.git("rev-parse", "HEAD^@").stdout.split()),
            2,
            "precondition: a real merge commit was created",
        )
        self.assertLedgerHas(h, 1, "the ungated merge must be recorded exactly once")
        self.assertTrue(
            h.ledger_lines()[0].startswith(head), "the entry is the new merge"
        )

    def test_reflog_action_saying_fast_forward_does_not_hide_it(self):
        h = self.harness()
        self._prepare(h)
        h.env["GIT_REFLOG_ACTION"] = "evil: Fast-forward"
        try:
            h.git("merge", "--no-ff", "--no-verify", "-m", "ungated merge", "feat")
        finally:
            del h.env["GIT_REFLOG_ACTION"]
        self._assert_one_entry_for_new_merge(h)

    def test_branch_and_message_named_fast_forward_do_not_hide_it(self):
        h = self.harness()
        self._prepare(h, branch="Fast-forward")
        h.git("merge", "--no-ff", "--no-verify", "-m", "x: Fast-forward",
              "Fast-forward")
        self._assert_one_entry_for_new_merge(h)

    def test_recorded_without_any_reflog(self):
        h = self.harness()
        self._prepare(h)
        h.git("config", "core.logAllRefUpdates", "false")
        shutil.rmtree(str(h.root / ".git" / "logs"))
        self.assertFalse((h.root / ".git" / "logs").exists(), "precondition")
        h.git("merge", "--no-ff", "--no-verify", "-m", "ungated merge", "feat")
        self._assert_one_entry_for_new_merge(h)


class ConflictResolvedMerge(GateTestCase):
    def test_conflicted_merge_concluded_by_commit_records_nothing(self):
        h = self.harness()
        h.git("checkout", "-q", "-b", "feat")
        h.write("c.txt", "feat\n")
        h.git("add", "-A")
        h.commit("feat side")
        h.git("checkout", "-q", "main")
        h.write("c.txt", "main\n")
        h.git("add", "-A")
        h.commit("main side")
        self.assertLedgerEmpty(h, "before the merge")

        proc = h.git("merge", "--no-ff", "-m", "merge feat", "feat", check=False)
        self.assertNotEqual(proc.returncode, 0, "precondition: the merge conflicts")
        h.write("c.txt", "resolved\n")
        h.git("add", "c.txt")
        h.clear_log()
        h.git("commit", "-q", "--no-edit")
        self.assertTrue(h.ran(), "precondition: the gate ran on the conclusion")
        self.assertEqual(
            len(h.git("rev-parse", "HEAD^@").stdout.split()),
            2,
            "precondition: the conclusion is a merge commit",
        )
        self.assertLedgerEmpty(
            h, "a conflicted merge concluded through the gate is not a bypass"
        )


if __name__ == "__main__":
    unittest.main()
