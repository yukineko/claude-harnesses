#!/usr/bin/env python3
"""RED repro (backlog 4f3da889): check-test-weakening refuses (Undetermined, exit 2) every change
to a Rust test file that exists at the base and holds a `macro_rules!` test
template, other than the narrow proven-safe shapes -- so a legitimate fix can
never be committed.

Observed 2026-11-07 (user ruling: fix the gate): the fix for
crates/blastguard/tests/heredoc_body_gt_fp_20261107.rs must change one
`target!(name, "<cmd>")` invocation's command-string argument and add one
`control!(name, check, "<cmd>")` invocation. scan() raises (78933b13, around
lines 1657-1689):

    #[test] inside a macro_rules! template in a file that exists at the base
    -- macro invocations are not counted, so removed generated tests cannot
    be ruled out

Contract specified here (file exists at the merge-base, template body
byte-identical, invocations at top level as in the blastguard file):
  RED  * changing an existing invocation's arguments, invocation count
         unchanged, yields a verdict (no Undetermined) and it is clean;
  RED  * adding a new invocation is clean;
  RED  * both together (the blastguard fix shape) is clean.
  KEEP * removing an invocation, or renaming one away to a macro that is not
         the template (so the template's invocation count drops), is NOT
         clean. Asserted as rc in {1, 2}, i.e. block or Undetermined.
         The coordinator's spec says rc 1 with a test-removed-like finding,
         but scripts/test_ctw_macro_rules_existing_file.py::
         test_removed_macro_invocation_is_undetermined freezes rc 2 for the
         same removal. This file does not choose between them. Tightening
         to rc 1 needs that frozen assert re-scoped first.
  KEEP * changing the template body (dropping the assert inside the macro,
         or inserting `return;`) is NOT clean (rc in {1, 2}).
  PIN  * ambiguous shapes stay Undetermined (rc 2): an invocation emitted
         from inside ANOTHER macro's body (macro-invoked-by-macro), and a
         template defined inside another macro_rules! (nested). Their
         arguments changing cannot be attributed by a top-level invocation
         count.

Set CTW_SCRIPT to run against another copy of the gate.

Repro:  python3 -m pytest --runxfail scripts/test_ctw_macro_rules_invocation_args.py
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(os.environ.get(
    "CTW_SCRIPT", str(Path(__file__).resolve().parent / "check-test-weakening.py")))

REL = "crates/demo/tests/heredoc_like.rs"

# Mirrors the shapes in crates/blastguard/tests/heredoc_body_gt_fp_20261107.rs:
# two templates (2-arg `target!`, 3-arg `control!`), multi-line top-level
# invocations whose last argument is an escaped command string.
TEMPLATES = """\
struct Fixture;

impl Fixture {
    fn new(_n: &str) -> Fixture {
        Fixture
    }
    fn assert_allow_both(&self, cmd: &str) {
        assert!(!cmd.is_empty(), "allow {cmd}");
    }
    fn assert_deny_both(&self, cmd: &str) {
        assert!(!cmd.is_empty(), "deny {cmd}");
    }
}

macro_rules! target {
    ($name:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            Fixture::new(stringify!($name)).assert_allow_both($cmd);
        }
    };
}

macro_rules! control {
    ($name:ident, $check:ident, $cmd:expr) => {
        #[test]
        fn $name() {
            Fixture::new(stringify!($name)).$check($cmd);
        }
    };
}
"""

INV_T1 = """
target!(
    target_cd_var_git_commit_heredoc,
    "WT=/private/tmp/w; cd \\"$WT\\" && git commit -q -F - <<'EOF'\\nfix: a -> b\\nEOF"
);
"""
INV_T1_ARGS_CHANGED = """
target!(
    target_cd_var_git_commit_heredoc,
    "WT=/private/tmp/w; cd \\"$WT\\" && git add -A && git commit -q -F - <<'EOF'\\nfix: a -> b\\n\\n> quoted\\nEOF"
);
"""
INV_T2 = """
target!(
    target_python_gt_no_space,
    "python3 - <<'EOF'\\nprint(3>2)\\nEOF"
);
"""
INV_C1 = """
control!(
    control_real_redirect_etc_hosts,
    assert_deny_both,
    "cat <<'EOF' > /etc/hosts\\nx\\nEOF"
);
"""
INV_C2_NEW = """
control!(
    control_nogt_python,
    assert_allow_both,
    "python3 - <<'EOF'\\nprint(3, 2)\\nEOF"
);
"""
# `target!` renamed away at the call site to a macro that is not the template.
INV_T2_RENAMED_AWAY = INV_T2.replace("target!(", "stringify!(", 1)

BASE = TEMPLATES + INV_T1 + INV_T2 + INV_C1

TEMPLATE_ASSERT_DROPPED = BASE.replace(
    "        assert!(!cmd.is_empty(), \"allow {cmd}\");\n", "", 1).replace(
    "            Fixture::new(stringify!($name)).assert_allow_both($cmd);\n",
    "            let _ = $cmd;\n", 1)
TEMPLATE_RETURN_INSERTED = BASE.replace(
    "        fn $name() {\n            Fixture::new(stringify!($name)).assert_allow_both($cmd);",
    "        fn $name() {\n            return;\n            Fixture::new(stringify!($name)).assert_allow_both($cmd);",
    1)

# Ambiguous shape 1: the template is invoked from inside another macro.
VIA_MACRO = TEMPLATES + """
macro_rules! cases {
    () => {
        target!(case_a, "echo a > /tmp/x");
        target!(case_b, "echo b");
    };
}

cases!();
"""
VIA_MACRO_ARGS_CHANGED = VIA_MACRO.replace('"echo a > /tmp/x"', '"echo a >> /tmp/x"', 1)

# Ambiguous shape 2: the test template is defined inside another macro_rules!.
NESTED = """\
macro_rules! make_templates {
    () => {
        macro_rules! inner {
            ($name:ident, $cmd:expr) => {
                #[test]
                fn $name() {
                    assert!(!$cmd.is_empty());
                }
            };
        }
    };
}

make_templates!();
inner!(nested_a, "echo a");
"""
NESTED_ARGS_CHANGED = NESTED.replace('"echo a"', '"echo aa"', 1)


class MacroRulesInvocationArgs(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="ctwmacroargs.")
        self.addCleanup(td.cleanup)
        self.tmp = Path(os.path.realpath(td.name))
        home = self.tmp / "home"
        home.mkdir()
        self.env = {
            "GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@example.invalid",
            "GIT_COMMITTER_NAME": "t", "GIT_COMMITTER_EMAIL": "t@example.invalid",
            "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_NOSYSTEM": "1",
            "HOME": str(home), "TMPDIR": str(self.tmp), "LC_ALL": "C",
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
        }
        self.repo = self.tmp / "repo"
        (self.repo / Path(REL).parent).mkdir(parents=True)

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def on_main_then_branch(self, base_src: str) -> None:
        """Commit `base_src` on main (so the file exists at the merge-base),
        then branch off."""
        self.git("init", "-q", "-b", "main")
        (self.repo / REL).write_text(base_src)
        self.git("add", "-A")
        self.git("-c", "core.hooksPath=/dev/null", "-c", "commit.gpgsign=false",
                 "commit", "-q", "-m", "C0 on main")
        self.git("checkout", "-q", "-b", "feat")

    def scan(self, after_src: str) -> tuple[int, str]:
        (self.repo / REL).write_text(after_src)
        self.git("add", "-A")
        self.assertIn(REL, self.git("diff", "--cached", "--name-only"),
                      "anti-vacuity: the change is staged")
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("commit\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, f"rc={r.returncode} stdout={r.stdout!r} stderr={r.stderr!r}"

    def assert_clean(self, rc: int, out: str) -> None:
        self.assertNotIn("undetermined", out, "must yield a verdict, not Undetermined")
        self.assertEqual(rc, 0, out)

    def assert_not_clean(self, rc: int, out: str) -> None:
        self.assertIn(rc, (1, 2), "must block or stay Undetermined, never clean: " + out)

    # -- anti-vacuity: the fixture really goes down the template path -----

    def test_fixture_is_seen_as_template_file(self) -> None:
        # Removing an invocation is refused today with the template reason.
        self.on_main_then_branch(BASE)
        rc, out = self.scan(TEMPLATES + INV_T1 + INV_C1)
        self.assertNotEqual(rc, 0, out)
        if rc == 2:
            self.assertIn("macro_rules! template", out,
                          "Undetermined must come from the template path")

    # -- RED ---------------------------------------------------------------

    @unittest.expectedFailure  # backlog 4f3da889: open defect
    def test_invocation_args_changed_is_clean(self) -> None:
        self.on_main_then_branch(BASE)
        rc, out = self.scan(TEMPLATES + INV_T1_ARGS_CHANGED + INV_T2 + INV_C1)
        self.assert_clean(rc, out)

    @unittest.expectedFailure  # backlog 4f3da889: open defect
    def test_invocation_added_is_clean(self) -> None:
        self.on_main_then_branch(BASE)
        rc, out = self.scan(BASE + INV_C2_NEW)
        self.assert_clean(rc, out)

    @unittest.expectedFailure  # backlog 4f3da889: open defect
    def test_blastguard_fix_shape_args_changed_and_control_added_is_clean(self) -> None:
        self.on_main_then_branch(BASE)
        rc, out = self.scan(TEMPLATES + INV_T1_ARGS_CHANGED + INV_T2 + INV_C1 + INV_C2_NEW)
        self.assert_clean(rc, out)

    # -- must stay not-clean -------------------------------------------------

    def test_invocation_removed_is_not_clean(self) -> None:
        self.on_main_then_branch(BASE)
        rc, out = self.scan(TEMPLATES + INV_T1 + INV_C1)
        self.assert_not_clean(rc, out)

    def test_invocation_renamed_away_is_not_clean(self) -> None:
        self.on_main_then_branch(BASE)
        rc, out = self.scan(TEMPLATES + INV_T1 + INV_T2_RENAMED_AWAY + INV_C1)
        self.assert_not_clean(rc, out)

    def test_removed_and_added_elsewhere_same_count_is_not_clean(self) -> None:
        # One target! dropped, one control! added: total count unchanged but the
        # target template's own count drops. Must not be laundered by the add.
        self.on_main_then_branch(BASE)
        rc, out = self.scan(TEMPLATES + INV_T1 + INV_C1 + INV_C2_NEW)
        self.assert_not_clean(rc, out)

    def test_template_assert_dropped_is_not_clean(self) -> None:
        self.on_main_then_branch(BASE)
        self.assertNotEqual(TEMPLATE_ASSERT_DROPPED, BASE, "anti-vacuity")
        rc, out = self.scan(TEMPLATE_ASSERT_DROPPED)
        self.assert_not_clean(rc, out)

    def test_template_return_inserted_is_not_clean(self) -> None:
        self.on_main_then_branch(BASE)
        self.assertNotEqual(TEMPLATE_RETURN_INSERTED, BASE, "anti-vacuity")
        rc, out = self.scan(TEMPLATE_RETURN_INSERTED)
        self.assert_not_clean(rc, out)

    # -- PIN: ambiguous shapes stay Undetermined ----------------------------

    def test_pin_invocation_via_another_macro_args_changed_is_undetermined(self) -> None:
        self.on_main_then_branch(VIA_MACRO)
        rc, out = self.scan(VIA_MACRO_ARGS_CHANGED)
        self.assertEqual(rc, 2, out)
        self.assertIn("undetermined", out)

    def test_pin_nested_template_args_changed_is_undetermined(self) -> None:
        self.on_main_then_branch(NESTED)
        rc, out = self.scan(NESTED_ARGS_CHANGED)
        self.assertEqual(rc, 2, out)
        self.assertIn("undetermined", out)


if __name__ == "__main__":
    unittest.main()
