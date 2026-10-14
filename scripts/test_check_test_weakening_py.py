#!/usr/bin/env python3
"""Independent verifier tests for the Python-test scanning of
scripts/check-test-weakening.py (backlog 911a93b8).

Stdlib only. Each test builds a throwaway git repo, commits a base test file,
rewrites it in the working tree, and runs the script as a subprocess with
`--repo <d> --base HEAD --json`.  Set CTW_SCRIPT to point at another copy of
the script (e.g. the pre-fix one) to observe RED.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = Path(os.environ.get("CTW_SCRIPT", str(HERE / "check-test-weakening.py")))
ENV = {
    "GIT_AUTHOR_NAME": "T", "GIT_AUTHOR_EMAIL": "t@example.invalid",
    "GIT_COMMITTER_NAME": "T", "GIT_COMMITTER_EMAIL": "t@example.invalid",
    "GIT_CONFIG_GLOBAL": os.devnull, "GIT_CONFIG_SYSTEM": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1", "HOME": "/nonexistent-home-for-tests", "LC_ALL": "C",
    "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
}

BASE = """\
import unittest


class T(unittest.TestCase):
    def test_a(self):
        self.assertEqual(f(), 1)
        self.assertTrue(g())

    def test_b(self):
        self.assertIn('x', h())
"""


def _git(d, *a):
    subprocess.run(["git", *a], cwd=d, env=ENV, check=True, capture_output=True, text=True)


class Base(unittest.TestCase):
    REL = "scripts/test_gate.py"

    def repo(self, base_src=BASE, rel=None):
        d = Path(tempfile.mkdtemp(prefix="ctw-py-"))
        self.addCleanup(shutil.rmtree, d, True)
        rel = rel or self.REL
        _git(d, "init", "-q")
        p = d / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(base_src)
        _git(d, "add", "-A")
        _git(d, "commit", "-q", "-m", "base")
        self.p = p
        self.rel = rel
        return d

    def run_gate(self, d, *extra):
        r = subprocess.run([sys.executable, str(SCRIPT), "--repo", str(d), "--base", "HEAD", "--json", *extra],
                           capture_output=True, text=True, cwd=d, env=ENV)
        try:
            j = json.loads(r.stdout)
        except Exception:
            j = {}
        return r.returncode, j, r

    def weaken(self, new_src, *extra, base_src=BASE):
        d = self.repo(base_src)
        self.p.write_text(new_src)
        return self.run_gate(d, *extra)

    def kinds(self, j):
        return sorted(f["kind"] for f in j.get("findings", []))

    def assertDetected(self, new_src, kind, base_src=BASE):
        rc, j, r = self.weaken(new_src, base_src=base_src)
        self.assertEqual(rc, 1, r.stdout + r.stderr)
        self.assertIn(kind, self.kinds(j), j)


class Detect(Base):
    def test_assertion_removed(self):
        self.assertDetected(BASE.replace("        self.assertTrue(g())\n", ""), "assertion-removed")

    def test_assertion_removed_bare_assert_statement(self):
        b = "def test_a():\n    assert f() == 1\n    assert g()\n"
        self.assertDetected("def test_a():\n    assert f() == 1\n", "assertion-removed", base_src=b)

    def test_assertion_moved_between_tests_is_still_weakening(self):
        # file total unchanged, but test_b lost its only assertion
        new = BASE.replace("        self.assertIn('x', h())\n", "        pass\n").replace(
            "        self.assertTrue(g())\n", "        self.assertTrue(g())\n        self.assertTrue(g2())\n")
        self.assertDetected(new, "assertion-removed")

    def test_test_method_removed(self):
        self.assertDetected(BASE.split("    def test_b")[0], "test-removed")

    def test_skip_decorator_added(self):
        self.assertDetected(BASE.replace("    def test_a", "    @unittest.skip('x')\n    def test_a"), "ignore-added")

    def test_skipif_decorator_added(self):
        self.assertDetected(BASE.replace("    def test_a", "    @unittest.skipIf(True, 'x')\n    def test_a"), "ignore-added")

    def test_expected_failure_added(self):
        self.assertDetected(BASE.replace("    def test_a", "    @unittest.expectedFailure\n    def test_a"),
                            "should-panic-added")

    def test_pytest_xfail_added(self):
        new = "import pytest\n" + BASE.replace("    def test_a", "    @pytest.mark.xfail\n    def test_a")
        self.assertDetected(new, "should-panic-added")

    def test_pytest_skip_mark_added(self):
        new = "import pytest\n" + BASE.replace("    def test_a", "    @pytest.mark.skip(reason='x')\n    def test_a")
        self.assertDetected(new, "ignore-added")

    def test_self_skiptest_in_body(self):
        self.assertDetected(BASE.replace("    def test_a(self):\n", "    def test_a(self):\n        self.skipTest('x')\n"),
                            "ignore-added")

    def test_pytest_skip_call_in_body(self):
        b = "import pytest\n\n\ndef test_a():\n    assert f()\n"
        self.assertDetected("import pytest\n\n\ndef test_a():\n    pytest.skip('x')\n    assert f()\n", "ignore-added", base_src=b)

    def test_plain_function_test_file_name_suffix(self):
        d = self.repo("def test_a():\n    assert f()\n", rel="pkg/gate_test.py")
        self.p.write_text("def test_a():\n    pass\n")
        rc, j, r = self.run_gate(d)
        self.assertEqual(rc, 1, r.stdout)

    def test_file_under_tests_dir_without_test_prefix(self):
        d = self.repo("def test_a():\n    assert f()\n", rel="pkg/tests/helpers.py")
        self.p.write_text("def test_a():\n    pass\n")
        rc, j, r = self.run_gate(d)
        self.assertEqual(rc, 1, r.stdout)

    # evasions that the implementation DOES catch
    def test_from_import_skip_alias(self):
        new = "from unittest import skip\n" + BASE.replace("    def test_a", "    @skip('x')\n    def test_a")
        self.assertDetected(new, "ignore-added")

    def test_module_alias_unittest_as_u(self):
        new = "import unittest as u\n" + BASE.replace("    def test_a", "    @u.skip('x')\n    def test_a")
        self.assertDetected(new, "ignore-added")

    def test_class_level_skip(self):
        self.assertDetected(BASE.replace("class T", "@unittest.skip('x')\nclass T"), "ignore-added")

    def test_module_level_pytestmark_skip(self):
        new = "import pytest\npytestmark = pytest.mark.skip(reason='x')\n" + BASE
        self.assertDetected(new, "ignore-added")

    def test_module_level_pytestmark_list_xfail(self):
        new = "import pytest\npytestmark = [pytest.mark.xfail]\n" + BASE
        self.assertDetected(new, "should-panic-added")

    def test_setup_skiptest(self):
        new = BASE.replace("    def test_a", "    def setUp(self):\n        self.skipTest('x')\n\n    def test_a", 1)
        self.assertDetected(new, "ignore-added")

    def test_raise_skiptest(self):
        self.assertDetected(BASE.replace("    def test_a(self):\n", "    def test_a(self):\n        raise unittest.SkipTest('x')\n"),
                            "ignore-added")


class Clean(Base):
    def test_unchanged_is_clean(self):
        d = self.repo()
        rc, j, r = self.run_gate(d)
        self.assertEqual(rc, 0, r.stdout + r.stderr)
        self.assertEqual(j.get("findings"), [])

    def test_adding_a_test_is_clean(self):
        rc, j, r = self.weaken(BASE + "\n    def test_c(self):\n        self.assertTrue(1)\n")
        self.assertEqual(rc, 0, r.stdout)

    def test_removing_a_skip_is_clean(self):
        b = BASE.replace("    def test_a", "    @unittest.skip('x')\n    def test_a")
        rc, j, r = self.weaken(BASE, base_src=b)
        self.assertEqual(rc, 0, r.stdout)
        b2 = BASE.replace("    def test_a", "    @unittest.expectedFailure\n    def test_a")
        rc, j, r = self.weaken(BASE, base_src=b2)
        self.assertEqual(rc, 0, r.stdout)

    def test_non_test_py_change_is_clean(self):
        d = self.repo("def f():\n    assert 1\n    assert 2\n", rel="scripts/helper.py")
        self.p.write_text("def f():\n    pass\n")
        rc, j, r = self.run_gate(d)
        self.assertEqual(rc, 0, r.stdout + r.stderr)

    def test_assertion_in_comment_or_string_not_counted_as_real(self):
        # removing a real assertion while adding a comment/string that says assert must still be flagged
        new = BASE.replace("        self.assertTrue(g())\n", "        # self.assertTrue(g())\n        s = 'self.assertTrue(g())'\n")
        self.assertDetected(new, "assertion-removed")


class FailClosed(Base):
    def test_unparseable_test_file_is_not_clean(self):
        rc, j, r = self.weaken("def test_a(:\n    pass\n")
        self.assertNotEqual(rc, 0, r.stdout + r.stderr)
        self.assertEqual(rc, 2, r.stdout + r.stderr)
        self.assertNotEqual(j.get("verdict"), "clean")


class Ack(Base):
    def _ack(self, d, text):
        m = d / "msg.txt"
        m.write_text(text)
        return self.run_gate(d, "--pending-msg", str(m))

    def test_ack_suppresses_exactly_the_acked_kind_for_that_path(self):
        d = self.repo()
        # two kinds at once: assertion removed + skip added
        self.p.write_text(BASE.replace("        self.assertTrue(g())\n", "").replace(
            "    def test_b", "    @unittest.skip('x')\n    def test_b"))
        rc, j, r = self.run_gate(d)
        self.assertEqual(rc, 1)
        self.assertEqual(self.kinds(j), ["assertion-removed", "ignore-added"], j)
        rc, j, r = self._ack(d, "m\n\ntest-weakening-justified: %s:assertion-removed - redundant\n" % self.rel)
        self.assertEqual(rc, 1, "the un-acked kind must still block: " + r.stdout)
        acked = {f["kind"]: f["acknowledged"] for f in j["findings"]}
        self.assertEqual(acked, {"assertion-removed": True, "ignore-added": False}, j)
        rc, j, r = self._ack(d, "m\n\ntest-weakening-justified: %s:assertion-removed - a\n"
                                "test-weakening-justified: %s:ignore-added - b\n" % (self.rel, self.rel))
        self.assertEqual(rc, 0, r.stdout + r.stderr)

    def test_ack_for_other_path_or_bare_marker_acks_nothing(self):
        d = self.repo()
        self.p.write_text(BASE.replace("        self.assertTrue(g())\n", ""))
        for msg in ("test-weakening-justified: scripts/other.py:assertion-removed - r\n",
                    "test-weakening-justified: %s:test-removed - r\n" % self.rel,
                    "test-weakening-justified:\n"):
            rc, j, r = self._ack(d, msg)
            self.assertEqual(rc, 1, msg + r.stdout)


if __name__ == "__main__":
    unittest.main()
