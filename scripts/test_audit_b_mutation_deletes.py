#!/usr/bin/env python3
"""Audit B item 19 (memory: fail-open-review-checklist §7, formerly
mutation-remove-dont-disable; user ruling 2026-11-07 on audit row #40).

Ruling: a mutant must DELETE the construct under test, never neutralize it in
place. `scripts/check-fail-open-mutation.py` still has a scenario whose `new`
text wraps the guarded block in `if false {` (the parallelguard cap check).
Code that can never execute is garbage (「絶対に処理されないコードはプログラムと
コンパイラのゴミ」), in mutants as much as in real code.

Property asserted: for every registered scenario, the mutant source generated
from the REAL target file (old -> new applied once, in memory; nothing is
written to disk) contains no more `if false` occurrences than the original.

Control: every scenario's anchor occurs exactly once in its real target file,
so the mutant really is generated (a missing anchor would make the property
vacuous).

Written by an auditor that does not implement the fix (CLAUDE.md §2(a)).
"""
import importlib.util
import re
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
REPO = _HERE.parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open_mutation_auditb", _HERE / "check-fail-open-mutation.py"
)
cfom = importlib.util.module_from_spec(_SPEC)
exec(  # noqa: S102
    compile(_SPEC.loader.get_source(_SPEC.name), _SPEC.origin, "exec", dont_inherit=True),
    cfom.__dict__,
)

# `if false` as a Rust condition: `if false {`, `if false &&`, `if false ||`.
DISABLED = re.compile(r"\bif\s+false\b")


def _mutant(scenario):
    original = (REPO / scenario.file_rel).read_text(encoding="utf-8")
    return original, original.replace(scenario.old, scenario.new, 1)


class MutantsDeleteRatherThanDisable(unittest.TestCase):
    def test_control_every_anchor_occurs_once(self):
        for s in cfom.SCENARIOS:
            with self.subTest(crate=s.crate):
                text = (REPO / s.file_rel).read_text(encoding="utf-8")
                self.assertEqual(text.count(s.old), 1, s.file_rel)

    @unittest.expectedFailure  # backlog d425cfc3: open defect
    def test_generated_mutant_contains_no_if_false(self):
        offenders = []
        for s in cfom.SCENARIOS:
            original, mutant = _mutant(s)
            added = len(DISABLED.findall(mutant)) - len(DISABLED.findall(original))
            if added > 0:
                offenders.append(f"{s.crate} ({s.file_rel}): +{added} `if false`")
        self.assertEqual(
            offenders,
            [],
            "mutants must delete the construct, not wrap it in `if false` "
            "(ruling 2026-11-07): " + "; ".join(offenders),
        )


if __name__ == "__main__":
    unittest.main()
