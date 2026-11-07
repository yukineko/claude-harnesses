#!/usr/bin/env python3
"""RED repro (2026-11-07): check-test-weakening must block a commit whose ONLY
change deletes one invocation `foo!(case_a);` of a `#[macro_export] macro_rules!`
test template that is DEFINED IN ANOTHER FILE (tests/support/macros.rs). The
deleted invocation removes a generated `#[test]`. Suspected gap: the gate only
reasons about templates defined in the same file as the invocation.

TARGET (expectedFailure until fixed): cross-file invocation removal blocks.
CONTROLS (must pass now): same-file invocation removal blocks; adding an
invocation in another file passes; editing an unrelated non-test line passes.

Repro: python3 -m pytest scripts/test_check_test_weakening_crossfile_macro_20261107.py -q
"""
from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-test-weakening.py"

MACRO_BODY = """\
macro_rules! foo {
    ($name:ident) => {
        #[test]
        fn $name() {
            let out = run();
            assert!(out.is_empty());
            assert_eq!(out.len(), 0);
        }
    };
}
"""
MACROS_RS = "#[macro_export]\n" + MACRO_BODY


def a_rs(invocations: list[str], note: str = "start") -> str:
    return ("#[macro_use]\nmod support;\n\n"
            f"// {note}\n" + "".join(f"foo!({n});\n" for n in invocations))


class CrossFileMacro(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twxfile.")
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
        self.tests = self.repo / "crates" / "c" / "tests"
        (self.tests / "support").mkdir(parents=True)
        self.git("init", "-q", "-b", "main")

    def git(self, *args: str) -> str:
        return subprocess.run(["git", *args], cwd=self.repo, env=self.env, check=True,
                              capture_output=True, text=True).stdout

    def write(self, files: dict[str, str]) -> None:
        for rel, content in files.items():
            (self.tests / rel).write_text(content)

    def base(self, files: dict[str, str]) -> None:
        self.write(files)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "C0")
        self.git("checkout", "-q", "-b", "feat")

    def change(self, files: dict[str, str]) -> None:
        self.write(files)
        self.git("add", "-A")
        self.assertTrue(self.git("diff", "--cached", "--name-only").strip(),
                        "anti-vacuity: the change is staged")

    def scan(self) -> tuple[int, str]:
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("change tests\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout + r.stderr

    # TARGET
    def test_target_crossfile_invocation_removal_blocks(self) -> None:
        self.base({"support/macros.rs": MACROS_RS, "a.rs": a_rs(["case_a", "case_b"])})
        self.change({"a.rs": a_rs(["case_b"])})
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, "RED: cross-file macro invocation removal passed: " + out)

    # CONTROLS
    def test_control_same_file_invocation_removal_blocks(self) -> None:
        body = lambda inv: MACRO_BODY + "".join(f"foo!({n});\n" for n in inv)
        self.base({"a.rs": body(["case_a", "case_b"])})
        self.change({"a.rs": body(["case_b"])})
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)

    def test_control_adding_invocation_in_other_file_passes(self) -> None:
        self.base({"support/macros.rs": MACROS_RS, "a.rs": a_rs(["case_a"]),
                   "b.rs": a_rs([])})
        self.change({"b.rs": a_rs(["case_c"])})
        rc, out = self.scan()
        self.assertEqual(rc, 0, out)

    def test_control_unrelated_non_test_line_edit_passes(self) -> None:
        self.base({"support/macros.rs": MACROS_RS, "a.rs": a_rs(["case_a"])})
        self.change({"a.rs": a_rs(["case_a"], note="edited comment")})
        rc, out = self.scan()
        self.assertEqual(rc, 0, out)


if __name__ == "__main__":
    unittest.main()
