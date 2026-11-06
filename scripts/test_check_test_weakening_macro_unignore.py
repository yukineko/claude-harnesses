#!/usr/bin/env python3
"""RED repro: check-test-weakening cannot clear the removal of an `#[ignore]`
inside a `macro_rules!` test template in a file that exists at the base.

Observed 2026-11-07 on main around 51f98352:

    check-test-weakening: undetermined — crates/blastguard/tests/git_config_worktree_twins_failopen_20261107.rs: #[test] inside a macro_rules! template in a file that exists at the base -- macro invocations are not counted, so removed generated tests cannot be ruled out
    test-weakening: UNDETERMINED (exit 2)

The Undetermined is raised before findings are computed, so a
`test-weakening-justified:` marker cannot clear it. The backlog protocol
(commit RED repros with `#[ignore = "backlog ..."]`, remove the ignore once the
fix lands) therefore cannot be completed for macro-generated tests.

Contract asserted (target): removing ONLY the `#[ignore]` line from the
template while the invocation count is unchanged is not a weakening; the gate
exits 0. Controls (pass today, must keep passing): dropping an invocation is
non-zero; adding an `#[ignore]` to the template is non-zero.

Repro:  python3 -m pytest scripts/test_check_test_weakening_macro_unignore.py::MacroUnignore::test_removing_template_ignore_with_same_invocations_passes
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"

TEMPLATE = """\
macro_rules! case {
    ($name:ident, $v:expr) => {
        #[test]
__IGNORE__        fn $name() {
            assert_eq!($v, 1);
        }
    };
}

"""
IGNORE_LINE = '        #[ignore = "backlog deadbeef: intentional RED repro"]\n'
INVOCATIONS = ["case!(a, 1);\n", "case!(b, 1);\n", "case!(c, 1);\n"]


def render(ignored: bool, invocations: list[str]) -> str:
    return TEMPLATE.replace("__IGNORE__", IGNORE_LINE if ignored else "") + "".join(invocations)


class MacroUnignore(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twmacro.")
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
        (self.repo / "crates" / "c" / "tests").mkdir(parents=True)
        self.git("init", "-q", "-b", "main")
        self.file = self.repo / "crates" / "c" / "tests" / "macro_repro.rs"

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def base(self, content: str) -> None:
        self.file.write_text(content)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "C0")
        self.git("checkout", "-q", "-b", "feat")

    def change(self, content: str) -> None:
        self.file.write_text(content)
        self.git("add", "-A")
        self.assertIn("macro_repro.rs", self.git("diff", "--cached", "--name-only"),
                      "anti-vacuity: the change is staged")

    def scan(self) -> tuple[int, str]:
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("change macro tests\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout + r.stderr

    def test_removing_template_ignore_with_same_invocations_passes(self) -> None:
        self.base(render(True, INVOCATIONS))
        self.change(render(False, INVOCATIONS))
        rc, out = self.scan()
        self.assertEqual(rc, 0,
                         "removing only the #[ignore] from a macro test template, "
                         "invocation count unchanged, restores coverage and must "
                         f"not be undetermined/blocked; output={out!r}")

    def test_control_dropping_an_invocation_is_not_clean(self) -> None:
        self.base(render(False, INVOCATIONS))
        self.change(render(False, INVOCATIONS[:-1]))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)

    def test_control_adding_template_ignore_is_not_clean(self) -> None:
        self.base(render(False, INVOCATIONS))
        self.change(render(True, INVOCATIONS))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)


if __name__ == "__main__":
    unittest.main()
