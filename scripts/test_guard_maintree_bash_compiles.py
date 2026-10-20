#!/usr/bin/env python3
"""guard-maintree-bash.py must compile with no warnings.

The hook runs on every Bash call. A SyntaxWarning (an invalid string escape)
prints on each run and becomes a SyntaxError in a future Python, which would
crash the hook (exit 2 on every call). Observed 2026-10-03 on main d1284de6:
`invalid escape sequence '\\`'` in the module docstring added by b5358f58.

Written by the ad524af9 worker (commit 6104ffca, class
CompilesWithoutWarnings); landed alone because the rest of ad524af9 is
superseded by the observing guard (b8390f7a). The docstring-text assertion
on the old static guard's escape fix (39268375) was dropped when the observing
guard replaced that docstring; the compile-under `-W error` property is kept.

    python3 -m unittest scripts.test_guard_maintree_bash_compiles
"""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

GUARD = Path(__file__).resolve().parent / "guard-maintree-bash.py"


class CompilesWithoutWarnings(unittest.TestCase):
    def test_guard_compiles_with_warnings_as_errors(self):
        r = subprocess.run(
            [sys.executable, "-W", "error", "-c",
             "import py_compile, sys; py_compile.compile(sys.argv[1], doraise=True)",
             str(GUARD)],
            capture_output=True, text=True,
            # compile only: keep the .pyc out of the source tree
            env=dict(os.environ, PYTHONPYCACHEPREFIX=tempfile.mkdtemp(prefix="guard-pyc-")),
        )
        self.assertEqual(r.returncode, 0, r.stderr[-800:])


if __name__ == "__main__":
    unittest.main()
