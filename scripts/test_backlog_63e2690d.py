#!/usr/bin/env python3
"""Repro for backlog 63e2690d: scripts/check-test-weakening.py counts an
`#[ignore]` that appears only inside a Rust COMMENT as an added ignore
attribute (`ignore-added`), and blocks a commit that weakened nothing.

Observable claim: a commit that adds a test file whose only `#[ignore]` text is
in a line comment (cargo reports 0 ignored) must be judged clean (exit 0) by
the gate. At the measured rev the gate exits 1 with `ignore-added`.

Open defect: the test is marked expectedFailure so the default suite stays
green. Run it RED with `python3 -m pytest --runxfail scripts/test_backlog_63e2690d.py`.
Remove the marker when the gate masks comments on the ignore count.
"""

import subprocess
import tempfile
import unittest
from pathlib import Path

import importlib.util

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location(
    "ctw_tests", _HERE / "test_check_test_weakening.py"
)
_ctw = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(_ctw)

COMMENT_ONLY_IGNORE_RS = """\
#[test]
fn bridge_supplies_evidence() {
    // A note for readers: do NOT add #[ignore] here; this test must run.
    assert_eq!(1 + 1, 2);
}
"""


class CommentIgnoreIsNotAnAttribute(_ctw.GateTestCase):
    @unittest.expectedFailure  # backlog 63e2690d: open defect, remove when fixed
    def test_new_test_file_with_ignore_only_in_a_comment_is_clean(self):
        repo = self.make_repo()
        repo.write("crates/demo/tests/bridge.rs", COMMENT_ONLY_IGNORE_RS)
        repo.commit("test: add a new test file (mentions #[ignore] in a comment only)")
        self.assertClean(*self.run_gate(repo.root))


if __name__ == "__main__":
    unittest.main()
