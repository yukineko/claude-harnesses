#!/usr/bin/env python3
"""RED repro (backlog ed82c49b): check-test-weakening accepts each proven-safe shape ALONE in a
`macro_rules!` test file that exists at the base, but not the two together.

The two shapes are:
  shape 1: inserted whole top-level items outside every template and fn body
           (`_only_proven_safe_line_changes`);
  shape 2: invocation-argument edits and added invocations
           (`template_invocation_edit_is_safe`, scripts/weakening_macro.py,
           backlog 4f3da889).
A change that is shape 1 plus shape 2 (a new top-level helper `fn` plus
invocation edits) is still Undetermined (rc 2) at main f145972f. That is the
shape of the blastguard regression fix fix-hdgt-regress2: two invocations'
arguments changed, one invocation added, and a new top-level helper fn.

Contract (same fixture and shapes as test_ctw_macro_rules_invocation_args.py,
file exists at the merge-base):
  RED      * two invocations' arguments changed, plus one invocation added,
             plus a new top-level `fn helper(..)` inserted between existing
             items: clean (rc 0, no Undetermined).
  CONTROL  * the helper insertion alone is clean, and the invocation edits
             alone are clean. This shows the combination is what is refused.
             The helper sits between `struct Fixture;` and `impl Fixture`.
  RED      * found while writing the control: placed right after an item that
             ends in `}` (here `impl Fixture { .. }`, the natural place for a
             helper), the helper is refused EVEN ALONE. difflib aligns the
             helper's closing `}` with the impl's, so the inserted block reads
             `}\\n\\nfn helper(..) {\\n    format!(..)\\n`. That block is
             unbalanced, so `_inserted_block_is_safe` rejects it. Shape 1's
             verdict thus depends on diff alignment, not on the change. Two
             tests cover it: that position alone, and that position plus the
             invocation edits. They are filed under the same backlog id as
             the combination, since a fix to how inserts are judged covers
             both; split them if the fix does not.
  KEEP     * the same, but the helper is added as a method inside the
             existing `impl` block (that modifies an existing item): rc 2;
           * the combination plus an invocation removed: rc 1 or 2;
           * the combination plus an edit to a template body: rc 2;
           * the combination where the inserted top-level item is itself a
             `macro_rules!` test template, or redefines an existing template
             name: rc 2.

Set CTW_SCRIPT to run against another copy of the gate.

Repro:  python3 -m pytest --runxfail scripts/test_ctw_macro_rules_helper_plus_invocations.py
"""
from __future__ import annotations

import importlib.util
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location(
    "ctw_macro_args", _HERE / "test_ctw_macro_rules_invocation_args.py")
m = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(m)

ANCHOR = "macro_rules! target {\n"
STRUCT = "struct Fixture;\n\n"

HELPER_FN = """\
fn helper(cmd: &str) -> String {
    format!("{cmd} ")
}

"""

# A new top-level item inserted between `struct Fixture;` and `impl Fixture`.
# At this position shape 1 ALONE is accepted (control below), so the RED
# isolates the combination.
TEMPLATES_WITH_HELPER = m.TEMPLATES.replace(STRUCT, STRUCT + HELPER_FN, 1)

# The same helper inserted right after `impl Fixture { .. }` (an item ending
# in `}`). difflib aligns the helper's closing `}` with the impl's, so the
# "inserted block" becomes `}\n\nfn helper(..) {\n    format!(..)\n` -- not
# balanced -- and shape 1 rejects it EVEN ALONE (observed at f145972f). This
# is the natural place for a helper; it is RED on its own, see header.
TEMPLATES_WITH_HELPER_AFTER_BRACE = m.TEMPLATES.replace(ANCHOR, HELPER_FN + ANCHOR, 1)

# The helper as a METHOD inside the existing impl block (modifies an item).
TEMPLATES_WITH_METHOD = m.TEMPLATES.replace(
    "impl Fixture {\n",
    "impl Fixture {\n    fn helper(cmd: &str) -> String {\n        format!(\"{cmd} \")\n    }\n",
    1)

# Inserted top-level item that is itself a test template.
NEW_TEMPLATE = """\
macro_rules! extra {
    ($name:ident) => {
        #[test]
        fn $name() {
            assert!(true);
        }
    };
}

"""
TEMPLATES_WITH_NEW_TEMPLATE = m.TEMPLATES.replace(STRUCT, STRUCT + HELPER_FN + NEW_TEMPLATE, 1)

# Inserted top-level item that redefines the existing `target` template name.
REDEFINED_TARGET = """\
macro_rules! target {
    ($name:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            let _ = $cmd;
        }
    };
}

"""
TEMPLATES_WITH_REDEFINITION = (
    m.TEMPLATES.replace(STRUCT, STRUCT + HELPER_FN + REDEFINED_TARGET, 1))

# Template body edited (target's body now routes through the helper).
TEMPLATES_WITH_HELPER_BODY_EDIT = TEMPLATES_WITH_HELPER.replace(
    "            Fixture::new(stringify!($name)).assert_allow_both($cmd);\n",
    "            Fixture::new(stringify!($name)).assert_allow_both(&helper($cmd));\n", 1)

INV_T2_ARGS_CHANGED = m.INV_T2.replace("print(3>2)", "print(3>2, 'x->y')", 1)

EDITED_INVOCATIONS = m.INV_T1_ARGS_CHANGED + INV_T2_ARGS_CHANGED + m.INV_C1 + m.INV_C2_NEW


class HelperPlusInvocations(unittest.TestCase):
    setUp = m.MacroRulesInvocationArgs.setUp
    git = m.MacroRulesInvocationArgs.git
    on_main_then_branch = m.MacroRulesInvocationArgs.on_main_then_branch
    scan = m.MacroRulesInvocationArgs.scan
    assert_clean = m.MacroRulesInvocationArgs.assert_clean
    assert_not_clean = m.MacroRulesInvocationArgs.assert_not_clean

    def test_anti_vacuity_fixture_shapes(self) -> None:
        for name, src in [("helper", TEMPLATES_WITH_HELPER),
                          ("helper after brace", TEMPLATES_WITH_HELPER_AFTER_BRACE),
                          ("method", TEMPLATES_WITH_METHOD),
                          ("new template", TEMPLATES_WITH_NEW_TEMPLATE),
                          ("redefinition", TEMPLATES_WITH_REDEFINITION),
                          ("body edit", TEMPLATES_WITH_HELPER_BODY_EDIT)]:
            self.assertNotEqual(src, m.TEMPLATES, f"{name}: replacement applied")
        self.assertNotEqual(TEMPLATES_WITH_HELPER_BODY_EDIT, TEMPLATES_WITH_HELPER)
        self.assertNotEqual(INV_T2_ARGS_CHANGED, m.INV_T2)

    # -- RED ---------------------------------------------------------------

    @unittest.expectedFailure  # backlog ed82c49b: open defect
    def test_helper_fn_plus_invocation_edits_is_clean(self) -> None:
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(TEMPLATES_WITH_HELPER + EDITED_INVOCATIONS)
        self.assert_clean(rc, out)

    @unittest.expectedFailure  # backlog ed82c49b: open defect
    def test_helper_fn_after_brace_item_alone_is_clean(self) -> None:
        # Shape 1 alone, helper placed right after `impl Fixture { .. }`.
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(TEMPLATES_WITH_HELPER_AFTER_BRACE + m.INV_T1 + m.INV_T2 + m.INV_C1)
        self.assert_clean(rc, out)

    @unittest.expectedFailure  # backlog ed82c49b: open defect
    def test_helper_fn_after_brace_item_plus_invocation_edits_is_clean(self) -> None:
        # The fix-hdgt-regress2 shape with the helper at its natural position.
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(TEMPLATES_WITH_HELPER_AFTER_BRACE + EDITED_INVOCATIONS)
        self.assert_clean(rc, out)

    # -- CONTROLS: each shape alone is accepted today ------------------------

    def test_control_helper_fn_alone_is_clean(self) -> None:
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(TEMPLATES_WITH_HELPER + m.INV_T1 + m.INV_T2 + m.INV_C1)
        self.assert_clean(rc, out)

    def test_control_invocation_edits_alone_are_clean(self) -> None:
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(m.TEMPLATES + EDITED_INVOCATIONS)
        self.assert_clean(rc, out)

    # -- KEEP: must stay not-clean -------------------------------------------

    def test_helper_as_method_in_existing_impl_plus_edits_is_undetermined(self) -> None:
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(TEMPLATES_WITH_METHOD + EDITED_INVOCATIONS)
        self.assertEqual(rc, 2, out)

    def test_combination_plus_invocation_removed_is_not_clean(self) -> None:
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(TEMPLATES_WITH_HELPER + m.INV_T1_ARGS_CHANGED
                            + INV_T2_ARGS_CHANGED + m.INV_C2_NEW)
        self.assert_not_clean(rc, out)

    def test_combination_plus_template_body_edit_is_undetermined(self) -> None:
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(TEMPLATES_WITH_HELPER_BODY_EDIT + EDITED_INVOCATIONS)
        self.assertEqual(rc, 2, out)

    def test_combination_with_inserted_template_is_undetermined(self) -> None:
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(TEMPLATES_WITH_NEW_TEMPLATE + EDITED_INVOCATIONS)
        self.assertEqual(rc, 2, out)

    def test_combination_with_redefined_template_name_is_undetermined(self) -> None:
        self.on_main_then_branch(m.BASE)
        rc, out = self.scan(TEMPLATES_WITH_REDEFINITION + EDITED_INVOCATIONS)
        self.assertEqual(rc, 2, out)


if __name__ == "__main__":
    unittest.main()
