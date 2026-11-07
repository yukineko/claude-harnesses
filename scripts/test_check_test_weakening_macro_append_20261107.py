#!/usr/bin/env python3
"""RED repro (2026-11-07, third shape): check-test-weakening raises Undetermined
for a commit whose ONLY change to a test file holding a `macro_rules!` test
template (shape of crates/blastguard/tests/heredoc_body_gt_fp_20261107.rs) is
APPENDING a new standalone `#[test] fn` at the end. Every byte of the base file
survives unchanged (base is a prefix of the new file), so no invocation,
template body or assertion can have been removed; the gate must exit 0.

Observed verbatim in session bf349dd5 while committing a new RED test to that
file: "check-test-weakening: undetermined — crates/blastguard/tests/
heredoc_body_gt_fp_20261107.rs: #[test] inside a macro_rules! template in a
file that exists at the base -- macro invocations are not counted, so removed
generated tests cannot be ruled out".

Controls (non-zero today, must stay non-zero): (a) append a test AND remove
one macro invocation; (b) append a test AND delete an assert in the template.

Repro: python3 -m pytest scripts/test_check_test_weakening_macro_append_20261107.py -q
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
        fn $name() {
            let out = run($cmd);
            assert!(out.is_empty());
            assert_eq!(out.len(), 0);
        }
    };
}

"""
INVOCATIONS = ["target!(t_one, \"a\");\n", "target!(t_two, \"b\");\n",
               "target!(t_three, \"c\");\n"]
APPENDED = """
#[test]
fn appended_standalone() {
    let out = run("d");
    assert!(out.is_empty());
}
"""


def render(invocations: list[str], template: str = TEMPLATE, tail: str = "") -> str:
    return template + "".join(invocations) + tail


class MacroAppend(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twmacroapp.")
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
        msg.write_text("add a test\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout + r.stderr

    @unittest.expectedFailure  # backlog 836d0725: open defect
    def test_target_appending_a_standalone_test_passes(self) -> None:
        before = render(INVOCATIONS)
        after = render(INVOCATIONS, tail=APPENDED)
        self.assertTrue(after.startswith(before), "anti-vacuity: pure append")
        self.base(before)
        self.change(after)
        rc, out = self.scan()
        self.assertEqual(rc, 0, out)

    def test_control_a_append_plus_removed_invocation_blocks(self) -> None:
        self.base(render(INVOCATIONS))
        self.change(render(INVOCATIONS[:-1], tail=APPENDED))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)

    def test_control_b_append_plus_deleted_template_assert_blocks(self) -> None:
        self.base(render(INVOCATIONS))
        weaker = TEMPLATE.replace("            assert_eq!(out.len(), 0);\n", "")
        self.assertNotEqual(weaker, TEMPLATE)
        self.change(render(INVOCATIONS, weaker, tail=APPENDED))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)


if __name__ == "__main__":
    unittest.main()
