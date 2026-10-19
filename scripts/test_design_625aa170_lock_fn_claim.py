#!/usr/bin/env python3
"""Doc claim vs code: docs/design-625aa170-...md says the runtime-conflict
cross-check "runs under `overwatch::lock::LeaseLock::acquire(repo)`".  The code
path that actually does the cross-check (crates/overwatch/src/store.rs
record_changeset_and_detect) must call that exact LeaseLock function.

Plain-failing while the doc and code disagree (the code uses acquire_strict;
`acquire` is test-only / #[allow(dead_code)]).
"""

import re
import unittest

import pytest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DOC = ROOT / "docs" / "design-625aa170-runtime-conflict-detect-consensus-resolve.md"
STORE = ROOT / "crates" / "overwatch" / "src" / "store.rs"


def fn_body(src: str, name: str) -> str:
    m = re.search(r"^pub fn %s\b" % re.escape(name), src, re.M)
    assert m, f"fn {name} not found in store.rs"
    nxt = re.search(r"^(?:pub )?fn \w+", src[m.end():], re.M)
    end = m.end() + nxt.start() if nxt else len(src)
    return src[m.start():end]


class T(unittest.TestCase):
    @pytest.mark.xfail(strict=True, reason="backlog: design-625aa170 doc names LeaseLock::acquire; RED repro: python3 -m pytest --runxfail scripts/test_design_625aa170_lock_fn_claim.py")
    def test_doc_named_lock_fn_is_the_one_record_changeset_and_detect_calls(self):
        doc = DOC.read_text()
        line = next(
            (l for l in doc.splitlines() if "Cross-check is a shared-registry RMW" in l),
            None,
        )
        self.assertIsNotNone(line, "doc cross-check sentence not found")
        named = set(re.findall(r"LeaseLock::(\w+)", line))
        self.assertTrue(named, f"doc line names no LeaseLock::<fn>: {line!r}")
        body = fn_body(STORE.read_text(), "record_changeset_and_detect")
        called = set(re.findall(r"LeaseLock::(\w+)", body))
        self.assertTrue(called, "record_changeset_and_detect calls no LeaseLock::<fn>")
        self.assertEqual(
            named,
            called,
            f"doc names LeaseLock::{sorted(named)} but record_changeset_and_detect calls LeaseLock::{sorted(called)}",
        )


if __name__ == "__main__":
    unittest.main()
