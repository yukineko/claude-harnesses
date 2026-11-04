#!/usr/bin/env python3
"""RED repro: a #[cfg(test)] field named like a weak keyword must not swallow production code.

Found 2026-11-03 by the independent verifier (sonnet) of condukt run
run-20261103-233558-54331 task gh1513-test-weakening at worktree commit
ad38f0bd. `_ITEM_HEAD_WORDS` contains the weak keywords `default`, `union`,
`auto` and `macro_rules`, which are also legal identifiers. A #[cfg(test)]
struct field named that way is classified as an item head, which ignores `}`
and `,` and runs on to the next `{`: the body of the FOLLOWING production fn
becomes test surface, so removing an assertion from production code is
invisible (fail-open, CLAUDE.md 3).

Property pinned: code outside the annotated item is never test surface. For
these inputs the checker may return a surface without the production marker,
or raise Undetermined (block); it must not return the production body.
"""
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"


def _load():
    spec = importlib.util.spec_from_file_location("check_test_weakening", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


PATH = "crates/demo/src/lib.rs"
MARKER = "PRODUCTION_BODY"

CASES = {
    "default_field": "struct S { #[cfg(test)] default: u8, z: u8 }\nfn prod() { assert!(PRODUCTION_BODY); }\n",
    "union_field": "struct S { #[cfg(test)] union: u8 }\nfn prod() { assert!(PRODUCTION_BODY); }\n",
    "auto_field": "struct S { #[cfg(test)] auto: u8 }\nfn prod() { assert!(PRODUCTION_BODY); }\n",
    "macro_rules_field": "struct S { #[cfg(test)] macro_rules: u8 }\nfn prod() { assert!(PRODUCTION_BODY); }\n",
}


class WeakKeywordFieldsDoNotSwallowProduction(unittest.TestCase):
    def setUp(self):
        self.m = _load()

    def test_each_case(self):
        for name, src in CASES.items():
            with self.subTest(name=name):
                try:
                    surface = self.m.cfg_test_modules(src, PATH)
                except self.m.Undetermined:
                    continue  # blocking is an acceptable resolution
                self.assertNotIn(
                    MARKER, surface,
                    f"{name}: a production fn body became #[cfg(test)] surface "
                    f"(weakening it would be invisible); surface={surface!r}")


if __name__ == "__main__":
    unittest.main()
