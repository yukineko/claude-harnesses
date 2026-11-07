#!/usr/bin/env python3
"""RED repro (2026-11-07, second shape): check-test-weakening raises Undetermined
for a commit whose ONLY test change is deleting one
`#[ignore = "RED: quoted heredoc body > read as redirect"]` line inside a
`macro_rules! target { ... #[test] ... }` template (shape of
crates/blastguard/tests/heredoc_body_gt_fp_20261107.rs) in a file that exists
at the base. Removing an #[ignore] strengthens tests; the gate must exit 0.

Controls (non-zero today, must stay non-zero): (a) one macro invocation
removed; (b) an #[ignore] added inside the template; (c) an assert deleted
inside the template.

Repro: python3 -m pytest scripts/test_check_test_weakening_macro_unignore_20261107.py -q
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
macro_rules! target {
    ($name:ident, $cmd:expr) => {
        #[test]
__IGNORE__        fn $name() {
            let out = run($cmd);
            assert!(out.is_empty());
            assert_eq!(out.len(), 0);
        }
    };
}

"""
IGNORE_LINE = '        #[ignore = "RED: quoted heredoc body > read as redirect"]\n'
INVOCATIONS = ["target!(t_one, \"a\");\n", "target!(t_two, \"b\");\n",
               "target!(t_three, \"c\");\n"]


def render(ignored: bool, invocations: list[str], template: str = TEMPLATE) -> str:
    return template.replace("__IGNORE__", IGNORE_LINE if ignored else "") + "".join(invocations)


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

    @unittest.expectedFailure  # RED: Undetermined raised before findings exist (check-test-weakening.py ~1782-1793)
    def test_target_deleting_only_template_ignore_passes(self) -> None:
        self.base(render(True, INVOCATIONS))
        self.change(render(False, INVOCATIONS))
        rc, out = self.scan()
        self.assertEqual(rc, 0, out)

    def test_control_a_removing_one_invocation_blocks(self) -> None:
        self.base(render(False, INVOCATIONS))
        self.change(render(False, INVOCATIONS[:-1]))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)

    def test_control_b_adding_template_ignore_blocks(self) -> None:
        self.base(render(False, INVOCATIONS))
        self.change(render(True, INVOCATIONS))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)

    def test_control_c_deleting_template_assert_blocks(self) -> None:
        self.base(render(False, INVOCATIONS))
        weaker = TEMPLATE.replace("            assert_eq!(out.len(), 0);\n", "")
        self.assertNotEqual(weaker, TEMPLATE)
        self.change(render(False, INVOCATIONS, weaker))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)


if __name__ == "__main__":
    unittest.main()
