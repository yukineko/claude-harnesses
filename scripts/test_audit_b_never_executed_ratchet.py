#!/usr/bin/env python3
"""Audit B item 20 (user ruling 2026-11-07: 「絶対に処理されないコードはプログラムと
コンパイラのゴミ」; memory: fail-open-review-checklist §7).

A pre-commit check must refuse NEWLY ADDED never-executed constructs under
`crates/**/src` (`if false {`, `while false {`, an `if false &&` guard), as a
diff-scoped ratchet: occurrences already in the tree are counted, not refused,
and nothing new can be added.

Property asserted (scanner-name agnostic): some scanner wired into
`.githooks/pre-commit` exits non-zero on a staged diff that adds the construct,
while exiting 0 on the identical diff with a live condition (`flag`) in its
place. The differential is what ties the refusal to the construct; see
scripts/audit_b_precommit_fixture.py.

Ratchet guard (passes today, must keep passing): a file that ALREADY contains
`if false {` at the base commit, with an unrelated live edit staged, is not
refused by any scanner that accepts the clean control.
"""
import unittest

from scripts import audit_b_precommit_fixture as F

CRATE = "fixturecrate"
SRC = "src/lib.rs"
BASE = "pub fn f(flag: bool) -> u32 {\n    let mut n = 0;\n    n += 1;\n    n\n}\n"


def _with(block):
    return BASE.replace("    n += 1;\n", "    n += 1;\n" + block)


def _block(cond):
    return f"    {cond} {{\n        n += 2;\n    }}\n"


class NeverExecutedRatchet(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        # One live-condition control shared by the three dead-construct cases:
        # the diffs differ only in the condition text.
        cls.control = F.run_scanners(CRATE, SRC, BASE, _with(_block("if flag")))

    def _assert_refused(self, dead_cond):
        refusing, bad = F.refusing_scanners(
            CRATE, SRC, BASE, _with(_block(dead_cond)), self.control
        )
        ok = F.accepting(self.control)
        self.assertTrue(
            refusing,
            f"no pre-commit scanner refuses a newly added `{dead_cond} {{` in "
            f"crates/{CRATE}/{SRC}. Scanners accepting the live control: {ok}; "
            f"their exit codes on the dead diff: { {s: bad[s][0] for s in ok} }",
        )

    def test_control_some_scanner_accepts_live_diff(self):
        self.assertTrue(F.accepting(self.control), self.control)

    def test_added_if_false_block_is_refused(self):
        self._assert_refused("if false")

    def test_added_while_false_is_refused(self):
        self._assert_refused("while false")

    def test_added_if_false_and_guard_is_refused(self):
        self._assert_refused("if false && flag")

    def test_ratchet_preexisting_occurrence_is_not_refused(self):
        base = _with(_block("if false"))
        staged = base.replace("    n\n}", "    n += 3;\n    n\n}")
        clean = F.run_scanners(CRATE, SRC, BASE, BASE.replace("    n\n}", "    n += 3;\n    n\n}"))
        ok = F.accepting(clean)
        pre = F.run_scanners(CRATE, SRC, base, staged, only=set(ok))
        blocked = [s for s in ok if pre[s][0] != 0]
        self.assertEqual(blocked, [], "a pre-existing occurrence must be counted, not refused")


if __name__ == "__main__":
    unittest.main()
