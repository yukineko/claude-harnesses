"""Independent verifier test for check-workspace-tests.py --python-only (eca8dea7).

Written by the condukt verifier, not by the implementing worker. It pins the
exit-code contract of --python-only against fixture repositories:

  * a green python body exits 0, and cargo is NOT launched (a fake cargo on
    PATH drops a marker file if it is ever executed) and the output says so;
  * a failing suite, an import-error suite, a crashed suite (SIGKILL), a suite
    with unparseable output, and a scripts/ dir with no suites all exit
    NON-ZERO;
  * a hung suite past the (patched) deadline exits non-zero (RC_DEADLINE);
  * --python-only combined with another argument is refused (exit 5).

Set CWT_SCRIPT to point at another copy of check-workspace-tests.py (used to
observe RED against the base revision).
"""

import importlib.util
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = Path(os.environ.get("CWT_SCRIPT") or (HERE / "check-workspace-tests.py"))

GREEN = "import unittest\nclass T(unittest.TestCase):\n    def test_ok(self):\n        self.assertTrue(True)\n"
RED = "import unittest\nclass T(unittest.TestCase):\n    def test_bad(self):\n        self.assertEqual(1, 2)\n"
IMPORT_ERR = "import no_such_module_verifier_eca8dea7\n"
CRASH = "import os, signal\nos.kill(os.getpid(), signal.SIGKILL)\n"
GARBAGE = "import os, sys\nsys.stdout.write('hello there\\n')\nsys.stdout.flush()\nos._exit(0)\n"
HANG = "import time, unittest\nclass T(unittest.TestCase):\n    def test_hang(self):\n        time.sleep(60)\n"


def _load_module():
    spec = importlib.util.spec_from_file_location("cwt_verifier_eca8dea7", str(SCRIPT))
    mod = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(mod)
    return mod


class PythonOnlyContract(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="vf-eca8dea7-")).resolve()
        self.repo = self.tmp / "repo"
        (self.repo / "crates").mkdir(parents=True)
        (self.repo / "scripts").mkdir()
        bindir = self.tmp / "bin"
        bindir.mkdir()
        self.marker = self.tmp / "cargo-was-run"
        fake = bindir / "cargo"
        fake.write_text("#!/bin/sh\ntouch '%s'\nexit 1\n" % self.marker)
        fake.chmod(0o755)
        self.env = dict(os.environ)
        self.env["PATH"] = str(bindir) + os.pathsep + self.env.get("PATH", "")

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _suite(self, name, body):
        (self.repo / "scripts" / ("test_%s.py" % name)).write_text(body)

    def _run(self, *args):
        r = subprocess.run(
            [sys.executable, str(SCRIPT)] + list(args),
            cwd=str(self.repo), env=self.env, stdin=subprocess.DEVNULL,
            capture_output=True, text=True, timeout=300,
        )
        return r.returncode, r.stdout + r.stderr

    def test_green_suite_exits_zero_and_cargo_not_launched(self):
        self._suite("a", GREEN)
        rc, out = self._run("--python-only")
        self.assertEqual(rc, 0, out)
        self.assertFalse(self.marker.exists(), "cargo was launched in --python-only")
        self.assertIn("NOT run", out, out)
        self.assertIn("cargo", out)

    def test_failing_suite_nonzero(self):
        self._suite("a", GREEN)
        self._suite("b", RED)
        rc, out = self._run("--python-only")
        self.assertNotEqual(rc, 0, out)
        self.assertNotEqual(rc, 5, "must be a suite verdict, not an arg refusal: " + out)
        self.assertFalse(self.marker.exists())

    def test_import_error_suite_nonzero(self):
        self._suite("a", GREEN)
        self._suite("b", IMPORT_ERR)
        rc, out = self._run("--python-only")
        self.assertNotIn(rc, (0, 5), out)

    def test_crashed_suite_nonzero(self):
        self._suite("a", GREEN)
        self._suite("b", CRASH)
        rc, out = self._run("--python-only")
        self.assertNotIn(rc, (0, 5), out)

    def test_unparseable_suite_nonzero(self):
        self._suite("a", GREEN)
        self._suite("b", GARBAGE)
        rc, out = self._run("--python-only")
        self.assertNotIn(rc, (0, 5), out)

    def test_no_suites_nonzero(self):
        rc, out = self._run("--python-only")
        self.assertNotIn(rc, (0, 5), out)

    def test_extra_argument_refused(self):
        self._suite("a", GREEN)
        rc, out = self._run("--python-only", "--x")
        self.assertEqual(rc, 5, out)

    def test_deadline_nonzero_in_process(self):
        self._suite("a", HANG)
        mod = _load_module()
        mod.PYTHON_SUITE_DEADLINE = 2.0

        def boom(*a, **k):
            raise AssertionError("cargo body touched in --python-only")

        mod.resolve_cargo = boom
        mod.run_cargo_body = boom
        old = os.getcwd()
        try:
            rc = mod.main(["--python-only"], repo=str(self.repo))
        finally:
            os.chdir(old)
        self.assertEqual(rc, mod.RC_DEADLINE)


if __name__ == "__main__":
    unittest.main(verbosity=2)
