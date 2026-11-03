#!/usr/bin/env python3
"""RED repro: a #[cfg(test)] item whose head contains a depth-0 comma must keep its body.

Found 2026-11-03 by the independent verifier of condukt run
run-20261103-233558-54331 task gh1513-test-weakening (worktree commit
cce4a3b6). That fix made `_annotated_block` end an item at the first depth-0
`;`, `,` or `}`, tracking only `(` and `[` depth. A comma inside `<...>`
generics, a generic return type or a where-clause therefore ends the item
before its `{`, cfg_test_modules skips it, and a genuine `#[cfg(test)]` fn or
impl contributes NOTHING to the test surface: weakening an assertion inside
it is invisible (fail-open, CLAUDE.md 3).

The main-tree code (before that fix) captured these bodies via "next `{`",
so this test must pass at main and is a regression guard for the fix.
Property pinned: the body of every genuine #[cfg(test)] item with a block is
part of the test surface, whatever its head contains.
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

CASES = {
    "generic_fn": "#[cfg(test)]\nfn f<A, B>() { assert!(GENERIC_FN_BODY); }\n",
    "generic_impl": "#[cfg(test)]\nimpl<A, B> X<A, B> { fn t() { assert!(GENERIC_IMPL_BODY); } }\n",
    "generic_return": "#[cfg(test)]\nfn f() -> Map<K, V> { assert!(GENERIC_RETURN_BODY); }\n",
    "where_clause": "#[cfg(test)]\nfn f<T, U>() where T: A, U: B { assert!(WHERE_CLAUSE_BODY); }\n",
    "plain_mod": "#[cfg(test)]\nmod tests { fn t() { assert!(PLAIN_MOD_BODY); } }\n",
}


class GenericCfgTestItemsKeepTheirBody(unittest.TestCase):
    def setUp(self):
        self.m = _load()

    def test_each_case(self):
        for name, src in CASES.items():
            marker = src.split("assert!(")[1].split(")")[0]
            with self.subTest(name=name):
                surface = self.m.cfg_test_modules(src, PATH)
                self.assertIn(
                    marker, surface,
                    f"{name}: #[cfg(test)] item body missing from the test surface "
                    f"(weakening inside it would be invisible); surface={surface!r}")


if __name__ == "__main__":
    unittest.main()
