#!/usr/bin/env python3
"""Independent tests for four behaviours of refactor_proof.py that go beyond the base spec
(tested in test_refactor_proof.py), each with a NEGATIVE control so the behaviour cannot be
abused, plus a RED test for a known gap:

  1. `ordered`: rustfmt re-wrap tolerance (brackets/commas are separate tokens; a trailing comma
     ending a line is dropped when the next line starts with a closing bracket).
  2. both modes: an attribute line directly above a dropped `use`/`mod` line is dropped with it.
  3. `ordered`: an inline `mod NAME { BODY }` moved to NAME.rs counts as equal.
  4. `ordered`: a line replaced in place by itself modulo normalisation (visibility change) is
     not something <from> gained.
  GAP: a multi-line 1-tuple `(\\n x,\\n)` -> `(x)` changes semantics and must exit 1.

Run: python3 scripts/test_refactor_proof_extras.py
"""

import os
import sys
import unittest

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from test_refactor_proof import SRC, ToolTestCase, run_tool  # noqa: E402

FROM = SRC + "/a.rs"
TO = SRC + "/b.rs"


class OrderedBase(ToolTestCase):
    def ordered(self, commit, frm=FROM, to=TO):
        return run_tool("ordered", "--repo", str(self.repo), "--commit", commit,
                        "--from", frm, "--to", to)

    def multiset(self, base, head, *pathspecs):
        return run_tool("multiset", "--repo", str(self.repo), "--base", base, "--head", head,
                        "--", *pathspecs)


# ------------------------------------------------------------------ 1. re-wrap tolerance


class RewrapTests(OrderedBase):
    BASE = "fn keep() {}\n\nfn g(a: i32, b: i32) -> i32 {\n    a + b\n}\n"

    def test_wrap_with_added_trailing_comma_is_equal(self):
        self.commit({FROM: self.BASE})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod b;\n",
            TO: "fn g(\n    a: i32,\n    b: i32,\n) -> i32 {\n    a + b\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_control_real_token_change_hidden_in_rewrap_differs(self):
        self.commit({FROM: "fn keep() {}\n\nfn f() {\n    call(a, b);\n}\n"})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod b;\n",
            TO: "fn f() {\n    call(a,\n        c);\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_dropping_one_line_one_tuple_comma_differs(self):
        self.commit({FROM: "fn keep() {}\n\nfn f() {\n    let t = (x,);\n}\n"})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod b;\n",
            TO: "fn f() {\n    let t = (x);\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_extra_trailing_comma_mid_line_differs(self):
        # A comma that does not end a line before a closing bracket is significant.
        self.commit({FROM: "fn keep() {}\n\nfn f() {\n    call(a, b);\n}\n"})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod b;\n",
            TO: "fn f() {\n    call(a, b,);\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)


class OneTupleGapTests(OrderedBase):
    def test_red_multiline_one_tuple_to_parenthesised_differs(self):
        # `(\n x,\n)` is a 1-tuple, `(x)` is a parenthesised expression: a semantics change.
        self.commit({FROM: "fn keep() {}\n\nfn f() {\n    let t = (\n        x,\n    );\n}\n"})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod b;\n",
            TO: "fn f() {\n    let t = (x);\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1,
                         "multi-line 1-tuple became `(x)` but was accepted: " + p.stdout + p.stderr)


# ------------------------------------------------------------------ 2. attribute above dropped use/mod


class AttrTests(OrderedBase):
    def test_multiset_attr_above_added_use_is_dropped(self):
        base = self.commit({FROM: "fn f() {}\n"})
        head = self.commit({FROM: "#[cfg(test)]\nuse crate::b::g;\nfn f() {}\n"})
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_multiset_attr_above_added_mod_decl_is_dropped(self):
        base = self.commit({FROM: "fn f() {}\n"})
        head = self.commit({FROM: "#[cfg(test)]\nmod tests;\nfn f() {}\n"})
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_ordered_attr_above_added_mod_decl_in_from_is_not_a_gain(self):
        self.commit({FROM: "fn keep() {}\n\nfn g() {}\n"})
        c = self.commit({
            FROM: "fn keep() {}\n\n#[cfg(test)]\nmod b;\n",
            TO: "fn g() {}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_control_multiset_attr_removed_from_fn_differs(self):
        base = self.commit({FROM: "#[cfg(test)]\nfn t() {}\n"})
        head = self.commit({FROM: "fn t() {}\nuse crate::x;\n"})
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_multiset_attr_above_fn_moved_without_attr_differs(self):
        base = self.commit({FROM: "#[cfg(test)]\nfn t() {}\n"})
        head = self.commit({
            FROM: "mod b;\n",
            TO: "fn t() {}\n",
        })
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_ordered_attr_not_moved_with_fn_differs(self):
        self.commit({FROM: "fn keep() {}\n\n#[cfg(test)]\nfn t() {}\n"})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod b;\n",
            TO: "fn t() {}\n",  # attribute dropped on the way
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_ordered_attr_above_fn_kept_in_from_is_a_gain(self):
        # `#[cfg(test)]` newly placed above a real fn in <from> must still count as gained.
        self.commit({FROM: "fn keep() {}\n\nfn g() {}\n"})
        c = self.commit({
            FROM: "#[cfg(test)]\nfn keep() {}\n\nmod b;\n",
            TO: "fn g() {}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)


# ------------------------------------------------------------------ 3. inline mod -> file


class InlineModTests(OrderedBase):
    BASE = "fn keep() {}\n\nmod inner {\n    fn a() -> i32 {\n        1\n    }\n}\n"

    def test_inline_mod_moved_to_name_rs_is_equal(self):
        self.commit({FROM: self.BASE})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod inner;\n",
            SRC + "/inner.rs": "fn a() -> i32 {\n    1\n}\n",
        })
        p = self.ordered(c, to=SRC + "/inner.rs")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_inline_mod_moved_to_name_dir_mod_rs_is_equal(self):
        self.commit({FROM: self.BASE})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod inner;\n",
            SRC + "/inner/mod.rs": "fn a() -> i32 {\n    1\n}\n",
        })
        p = self.ordered(c, to=SRC + "/inner/mod.rs")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_control_inline_mod_body_changed_differs(self):
        self.commit({FROM: self.BASE})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod inner;\n",
            SRC + "/inner.rs": "fn a() -> i32 {\n    2\n}\n",
        })
        p = self.ordered(c, to=SRC + "/inner.rs")
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_inline_mod_name_differs_from_file_name(self):
        self.commit({FROM: self.BASE})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod inner;\n",
            SRC + "/other.rs": "fn a() -> i32 {\n    1\n}\n",
        })
        p = self.ordered(c, to=SRC + "/other.rs")
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_inline_mod_body_dropped_item_differs(self):
        self.commit({FROM: "fn keep() {}\n\nmod inner {\n    fn a() {}\n    fn b() {}\n}\n"})
        c = self.commit({
            FROM: "fn keep() {}\n\nmod inner;\n",
            SRC + "/inner.rs": "fn a() {}\n",
        })
        p = self.ordered(c, to=SRC + "/inner.rs")
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)


# ------------------------------------------------------------------ 4. same-position visibility change


class SamePositionTests(OrderedBase):
    BASE = "fn keep() {}\n\nfn g() -> i32 {\n    2\n}\n"

    def test_visibility_change_elsewhere_in_from_is_not_a_gain(self):
        self.commit({FROM: self.BASE})
        c = self.commit({
            FROM: "pub(super) fn keep() {}\n\nmod b;\n",
            TO: "fn g() -> i32 {\n    2\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_visibility_change_adjacent_to_removed_block_is_not_a_gain(self):
        self.commit({FROM: "fn keep() {}\nfn g() -> i32 {\n    2\n}\n"})
        c = self.commit({
            FROM: "pub(crate) fn keep() {}\nmod b;\n",
            TO: "fn g() -> i32 {\n    2\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_control_same_position_body_change_differs(self):
        self.commit({FROM: self.BASE})
        c = self.commit({
            FROM: "fn keep() { sneaky(); }\n\nmod b;\n",
            TO: "fn g() -> i32 {\n    2\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_same_position_rename_with_visibility_differs(self):
        self.commit({FROM: self.BASE})
        c = self.commit({
            FROM: "pub fn other() {}\n\nmod b;\n",
            TO: "fn g() -> i32 {\n    2\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_same_position_adjacent_change_differs(self):
        self.commit({FROM: "fn keep() {}\nfn g() -> i32 {\n    2\n}\n"})
        c = self.commit({
            FROM: "fn keep2() {}\nmod b;\n",
            TO: "fn g() -> i32 {\n    2\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)


if __name__ == "__main__":
    unittest.main()
