#!/usr/bin/env python3
"""Repro for backlog c31ab45d: crates/tdd/README.ja.md and
crates/donegate/README.ja.md still state the fail-open contract that d6db4670
falsified ("harness errors always allow the stop"). At HEAD a failed git scan
maps to Determination::undetermined and blocks (tdd gate.rs), an unreadable
donegate.toml routes to refuse() and a blocking undetermined verdict, and a
panic fails closed via harness_core::gate::run::run_guarded.

The control proves the contradiction is real at this rev: tdd's own main.rs
documents that it is deliberately NOT a never-break-the-turn guard.
"""
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TDD_JA = REPO / "crates/tdd/README.ja.md"
DONEGATE_JA = REPO / "crates/donegate/README.ja.md"


class JaReadmeFailOpenContract(unittest.TestCase):
    def test_control_code_documents_fail_closed(self):
        self.assertIn("fail closed",
                      (REPO / "crates/harness-core/src/gate/run.rs").read_text(encoding="utf-8"))
        self.assertIn("never break the turn",
                      (REPO / "crates/tdd/src/main.rs").read_text(encoding="utf-8"))

    @unittest.expectedFailure  # backlog c31ab45d: open defect, remove when fixed
    def test_tdd_ja_readme_does_not_promise_errors_allow_the_stop(self):
        t = TDD_JA.read_text(encoding="utf-8")
        bad = [l.strip() for l in t.splitlines()
               if "ターンを壊さない" in l or ("exit 0" in l and "停止を許す" in l)]
        self.assertEqual(bad, [], "tdd README.ja states the falsified fail-open contract:\n"
                         + "\n".join(bad))

    @unittest.expectedFailure  # backlog c31ab45d: open defect, remove when fixed
    def test_donegate_ja_readme_does_not_promise_errors_always_pass(self):
        t = DONEGATE_JA.read_text(encoding="utf-8")
        bad = [l.strip() for l in t.splitlines() if "常に終了コード 0 で停止を通す" in l]
        self.assertEqual(bad, [], "donegate README.ja states the falsified fail-open contract:\n"
                         + "\n".join(bad))


if __name__ == "__main__":
    unittest.main()
