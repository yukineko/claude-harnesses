#!/usr/bin/env python3
"""RED tests for crates/refactor/skills/refactor/scripts/refactor_proof.py (not yet implemented).

The tool generalises the blastguard-refactor one-off proofs (line_multiset.py, ordered_move.py,
bg-test-compare.sh). Exit codes for every subcommand: 0 = proven equal, 1 = differs,
2 = undetermined (never 0). Each test builds its own temp git repo and invokes the tool by
absolute path. Run: python3 -m unittest scripts.test_refactor_proof  (or python3 scripts/test_refactor_proof.py)
"""

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TOOL = REPO / "crates" / "refactor" / "skills" / "refactor" / "scripts" / "refactor_proof.py"


def run_tool(*args, cwd=None):
    # A missing tool file makes python exit 2, so every test first asserts the tool exists
    # (see ToolTestCase.setUp) -- otherwise the exit-2 tests would pass vacuously.
    return subprocess.run(
        [sys.executable, str(TOOL), *args], capture_output=True, text=True, cwd=cwd, timeout=120
    )


def git(repo, *args):
    p = subprocess.run(
        ["git", "-C", str(repo), "-c", "user.name=t", "-c", "user.email=t@example.com",
         "-c", "commit.gpgsign=false", *args],
        capture_output=True, text=True,
    )
    assert p.returncode == 0, "git %s failed: %s" % (" ".join(args), p.stderr)
    return p.stdout.strip()


class ToolTestCase(unittest.TestCase):
    def setUp(self):
        self.assertTrue(TOOL.is_file(), "tool missing: %s" % TOOL)
        self._td = tempfile.TemporaryDirectory()
        self.addCleanup(self._td.cleanup)
        self.tmp = Path(self._td.name)
        self.repo = self.tmp / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q")

    def commit(self, files, msg="c"):
        for rel, content in files.items():
            p = self.repo / rel
            p.parent.mkdir(parents=True, exist_ok=True)
            if content is None:
                p.unlink()
            else:
                p.write_text(content)
        git(self.repo, "add", "-A")
        git(self.repo, "commit", "-q", "-m", msg)
        return git(self.repo, "rev-parse", "HEAD")

    def multiset(self, base, head, *pathspecs):
        return run_tool("multiset", "--repo", str(self.repo), "--base", base, "--head", head,
                        "--", *pathspecs)


A_BEFORE = "use std::fmt;\n\nfn f() -> i32 {\n    1\n}\n\nfn g() -> i32 {\n    2\n}\n"
SRC = "crates/x/src"


class MultisetTests(ToolTestCase):
    def test_move_fn_to_new_file_with_mod_and_use_is_equal(self):
        base = self.commit({SRC + "/a.rs": A_BEFORE})
        head = self.commit({
            SRC + "/a.rs": "use std::fmt;\nmod b;\nuse crate::b::g;\n\nfn f() -> i32 {\n    1\n}\n",
            SRC + "/b.rs": "//! moved\nuse std::fmt;\n\nfn g() -> i32 {\n    2\n}\n",
        })
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertRegex(p.stdout, r"\d", "expected a summary line with file/line counts")

    def test_visibility_change_is_tolerated(self):
        base = self.commit({SRC + "/a.rs": A_BEFORE})
        head = self.commit({SRC + "/a.rs": A_BEFORE.replace("fn f()", "pub(super) fn f()")})
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_multiline_use_and_pub_mod_are_dropped(self):
        base = self.commit({SRC + "/a.rs": "fn f() {}\n"})
        head = self.commit({SRC + "/a.rs": "pub(crate) use foo::{\n    a,\n    b,\n};\npub mod m;\nfn f() {}\n"})
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_control_changed_literal_differs_and_shows_both_lines(self):
        base = self.commit({SRC + "/a.rs": A_BEFORE})
        head = self.commit({SRC + "/a.rs": A_BEFORE.replace("    1\n", "    3\n")})
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("1", p.stdout)
        self.assertIn("3", p.stdout)

    def test_control_duplicated_line_differs(self):
        base = self.commit({SRC + "/a.rs": A_BEFORE})
        head = self.commit({
            SRC + "/a.rs": A_BEFORE,
            SRC + "/b.rs": "fn g() -> i32 {\n    2\n}\n",  # copy without removal
        })
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_pathspec_matching_no_files_is_undetermined(self):
        base = self.commit({"other/a.rs": "fn f() {}\n"})
        head = self.commit({"other/b.txt": "x\n"})
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)

    def test_pathspec_missing_at_one_rev_is_undetermined(self):
        base = self.commit({"other/a.rs": "fn f() {}\n"})
        head = self.commit({SRC + "/a.rs": "fn f() {}\n"})
        p = self.multiset(base, head, SRC)
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)

    def test_bad_rev_is_undetermined(self):
        head = self.commit({SRC + "/a.rs": "fn f() {}\n"})
        p = self.multiset("deadbeefdeadbeefdeadbeefdeadbeefdeadbeef", head, SRC)
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)


class OrderedTests(ToolTestCase):
    FROM = SRC + "/a.rs"
    TO = SRC + "/b.rs"

    def ordered(self, commit, frm=None, to=None):
        return run_tool("ordered", "--repo", str(self.repo), "--commit", commit,
                        "--from", frm or self.FROM, "--to", to or self.TO)

    def setUp(self):
        super().setUp()
        self.commit({self.FROM: "use std::fmt;\n\nfn keep() {}\n\nfn g() -> i32 {\n    2\n}\n\nfn h() -> i32 {\n    3\n}\n"})

    def test_ordered_move_is_equal_even_when_rewrapped(self):
        c = self.commit({
            self.FROM: "use std::fmt;\nmod b;\nuse crate::b::{g, h};\n\nfn keep() {}\n",
            # rustfmt-style re-wrap of the signature must be tolerated
            self.TO: "//! m\nuse std::fmt;\n\npub(super) fn g()\n    -> i32 {\n    2\n}\n\nfn h() -> i32 { 3 }\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_control_swapped_functions_differ(self):
        c = self.commit({
            self.FROM: "use std::fmt;\nmod b;\n\nfn keep() {}\n",
            self.TO: "fn h() -> i32 {\n    3\n}\n\nfn g() -> i32 {\n    2\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_control_from_gains_non_use_line_differs(self):
        c = self.commit({
            self.FROM: "use std::fmt;\nmod b;\n\nfn keep() {}\nfn sneaky() {}\n",
            self.TO: "fn g() -> i32 {\n    2\n}\n\nfn h() -> i32 {\n    3\n}\n",
        })
        p = self.ordered(c)
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_to_absent_at_commit_is_undetermined(self):
        c = self.commit({self.FROM: "use std::fmt;\n\nfn keep() {}\n"})
        p = self.ordered(c)
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)


FAKE = """\
import pathlib, sys
d = pathlib.Path(sys.argv[1])
sys.stdout.write((d / "out.txt").read_text())
err = d / "err.txt"
if err.exists():
    sys.stderr.write(err.read_text())
rc = d / "rc.txt"
sys.exit(int(rc.read_text()) if rc.exists() else 0)
"""

BASE_OUT = (
    "running 4 tests\n"
    "test a::one ... ok\n"
    "test a::two ... ok\n"
    "test a::bad ... FAILED\n"
    "test a::skip ... ignored\n"
    "\ntest result: FAILED. 2 passed; 1 failed; 1 ignored; 0 measured; 0 filtered out\n"
)


class TestsSubcommand(ToolTestCase):
    def setUp(self):
        super().setUp()
        self.commit({"README": "x\n"})
        self.data = self.tmp / "data"
        self.data.mkdir()
        self.fake = self.tmp / "fake.py"
        self.fake.write_text(FAKE)
        self.baseline = self.tmp / "baseline"
        self.set_output(BASE_OUT)

    def set_output(self, out, err=None, rc=None):
        (self.data / "out.txt").write_text(out)
        for name, val in (("err.txt", err), ("rc.txt", None if rc is None else str(rc))):
            p = self.data / name
            if val is None:
                if p.exists():
                    p.unlink()
            else:
                p.write_text(val)

    def cmd(self):
        return [sys.executable, str(self.fake), str(self.data)]

    def record(self):
        return run_tool("tests", "record", "--repo", str(self.repo), "--out", str(self.baseline),
                        "--", *self.cmd())

    def compare(self, allow=None):
        extra = ["--allow-removed", str(allow)] if allow else []
        return run_tool("tests", "compare", "--repo", str(self.repo), "--baseline", str(self.baseline),
                        *extra, "--", *self.cmd())

    def test_record_writes_sorted_pass_and_fail_sets(self):
        p = self.record()
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual((self.baseline / "pass.txt").read_text().split(), ["a::one", "a::two"])
        self.assertEqual((self.baseline / "fail.txt").read_text().split(), ["a::bad"])

    def test_same_output_is_equal(self):
        self.assertEqual(self.record().returncode, 0)
        p = self.compare()
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_stderr_is_not_parsed(self):
        self.assertEqual(self.record().returncode, 0)
        self.set_output(BASE_OUT, err="test bogus ... wat\ntest a::one ... FAILED\n")
        p = self.compare()
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_nonzero_exit_with_identical_parse_is_equal(self):
        self.assertEqual(self.record().returncode, 0)
        self.set_output(BASE_OUT, rc=101)
        p = self.compare()
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_control_pass_becoming_failed_differs(self):
        self.assertEqual(self.record().returncode, 0)
        self.set_output(BASE_OUT.replace("test a::two ... ok", "test a::two ... FAILED"))
        p = self.compare()
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("a::two", p.stdout)

    def test_control_new_passing_test_differs(self):
        self.assertEqual(self.record().returncode, 0)
        self.set_output(BASE_OUT.replace("test a::two ... ok\n", "test a::two ... ok\ntest a::three ... ok\n"))
        p = self.compare()
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("a::three", p.stdout)

    def test_control_removed_test_without_allowance_differs(self):
        self.assertEqual(self.record().returncode, 0)
        self.set_output(BASE_OUT.replace("test a::two ... ok\n", ""))
        p = self.compare()
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)
        self.assertIn("a::two", p.stdout)

    def test_allowed_removal_is_equal(self):
        self.assertEqual(self.record().returncode, 0)
        self.set_output(BASE_OUT.replace("test a::two ... ok\n", ""))
        allow = self.tmp / "allow.txt"
        allow.write_text("a::two\n")
        p = self.compare(allow)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)

    def test_no_test_result_line_is_undetermined(self):
        self.assertEqual(self.record().returncode, 0)
        self.set_output("test a::one ... ok\ntest a::two ... ok\ntest a::bad ... FAILED\n")
        p = self.compare()
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)

    def test_garbled_test_line_is_undetermined(self):
        self.assertEqual(self.record().returncode, 0)
        self.set_output(BASE_OUT.replace("test a::two ... ok", "test a::two ... wat"))
        p = self.compare()
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)

    def test_record_with_no_test_result_is_undetermined(self):
        self.set_output("compiling...\nerror: build failed\n", rc=101)
        p = self.record()
        self.assertEqual(p.returncode, 2, p.stdout + p.stderr)


if __name__ == "__main__":
    unittest.main()
