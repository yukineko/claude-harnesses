#!/usr/bin/env python3
"""RED repro: check-test-weakening returns Undetermined (exit 2) for an
ADDITIVE-ONLY edit to a file whose base contains a `#[test]` inside a
`macro_rules!` template.

Observed 2026-11-07 (blastguard test-env fix): adding one `.env(...)` line to
crates/blastguard/tests/git_config_worktree_twins_failopen_20261107.rs, with no
macro invocation removed or changed, was blocked:

    check-test-weakening: undetermined -- ...: #[test] inside a macro_rules!
    template in a file that exists at the base -- macro invocations are not
    counted, so removed generated tests cannot be ruled out

An edit that only adds a non-test line cannot remove a generated test, so the
gate has nothing undetermined to say.

Contract asserted (target): base has the templated test and N invocations; the
change adds exactly one non-test line and leaves every template/invocation line
untouched; the gate exits 0.
Control (passes today, must keep passing): dropping an invocation is non-zero.

Repro:  python3 -m pytest scripts/test_check_test_weakening_macro_additive.py::MacroAdditive::test_additive_non_test_line_is_not_undetermined
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
        fn $name() {
            assert_eq!($v, 1);
        }
    };
}

"""
INVOCATIONS = ["case!(a, 1);\n", "case!(b, 1);\n", "case!(c, 1);\n"]
HELPER = "fn helper() -> u32 {\n    1\n}\n\n"


class MacroAdditive(unittest.TestCase):
    def setUp(self) -> None:
        td = tempfile.TemporaryDirectory(prefix="twadd.")
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
        self.file = self.repo / "crates" / "c" / "tests" / "macro_additive.rs"

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
        self.assertIn("macro_additive.rs", self.git("diff", "--cached", "--name-only"),
                      "anti-vacuity: the change is staged")

    def scan(self) -> tuple[int, str]:
        msg = self.tmp / "COMMIT_EDITMSG"
        msg.write_text("add a helper\n")
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(self.repo),
                            "--pending-msg", str(msg)],
                           cwd=self.repo, env=self.env, capture_output=True,
                           text=True, timeout=60)
        return r.returncode, r.stdout + r.stderr

    def test_additive_non_test_line_is_not_undetermined(self) -> None:
        body = TEMPLATE + "".join(INVOCATIONS)
        self.base(body)
        self.change(body + "\n" + HELPER)
        diff = self.git("diff", "--cached", "-U0")
        removed = [l for l in diff.splitlines() if l.startswith("-") and not l.startswith("---")]
        self.assertEqual(removed, [], "anti-vacuity: the diff removes nothing")
        rc, out = self.scan()
        self.assertEqual(rc, 0,
                         "an edit that only ADDS a non-test line cannot remove a "
                         f"generated test and must not be undetermined; rc={rc} output={out!r}")

    def test_control_dropping_an_invocation_is_not_clean(self) -> None:
        self.base(TEMPLATE + "".join(INVOCATIONS))
        self.change(TEMPLATE + "".join(INVOCATIONS[:-1]))
        rc, out = self.scan()
        self.assertNotEqual(rc, 0, out)


if __name__ == "__main__":
    unittest.main()
