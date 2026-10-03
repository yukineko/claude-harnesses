#!/usr/bin/env python3
"""Repro for backlog 7b88aa9a: docs/DESIGN-spec-loop.md section 1 ("いま何が
繋がっていないか") states in the present tense that a ratified spec has NO path
into the queue ("批准された spec が queue に入る経路は**無い**"), while
crates/specguard/src/forge/main.rs implements one: `ratify` ends in
`enqueue_and_report`, which calls `queue::enqueue_with`.

The test checks the code fact first (so it cannot pass because the code went
away), then requires the doc to stop asserting the absence, or at least to name
the implementing path (`enqueue_and_report` / `forge/queue.rs`) next to it.

Open defect: expectedFailure keeps the default suite green. Run it RED with
`python3 -m pytest --runxfail scripts/test_backlog_7b88aa9a.py`.
"""

import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
DOC = REPO / "docs" / "DESIGN-spec-loop.md"
FORGE_MAIN = REPO / "crates" / "specguard" / "src" / "forge" / "main.rs"
FORGE_QUEUE = REPO / "crates" / "specguard" / "src" / "forge" / "queue.rs"

ABSENCE_CLAIM = "queue に入る経路は**無い**"


class DesignDocMatchesRatifyQueuePath(unittest.TestCase):
    def test_code_has_a_ratify_to_queue_path(self):
        """Control (not RED-marked): the path the doc denies does exist."""
        src = FORGE_MAIN.read_text(encoding="utf-8")
        ratify = re.search(r"fn ratify\(.*?\n}\n", src, re.S)
        self.assertIsNotNone(ratify, "fn ratify not found in forge/main.rs")
        self.assertIn("enqueue_and_report(", ratify.group(0))
        self.assertIn("queue::enqueue_with(", src)
        self.assertTrue(FORGE_QUEUE.exists())

    @unittest.expectedFailure  # backlog 7b88aa9a: open defect, remove when fixed
    def test_doc_does_not_deny_the_existing_path(self):
        doc = DOC.read_text(encoding="utf-8")
        if ABSENCE_CLAIM in doc:
            self.assertTrue(
                "enqueue_and_report" in doc or "forge/queue.rs" in doc,
                f"{DOC.relative_to(REPO)} says {ABSENCE_CLAIM!r} but never names "
                "the ratify -> enqueue_and_report -> queue::enqueue_with path the "
                "code implements",
            )


if __name__ == "__main__":
    unittest.main()
