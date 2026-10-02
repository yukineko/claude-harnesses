#!/usr/bin/env python3
"""RED repro for backlog 4ffc77b5.

`crates/fugu-router/tests/code_index.rs::seeded_repo` creates
`$TMPDIR/fugu-router-code-index-<tag>-<pid>-<nanos>` for every test and nothing
removes it, so every test run leaks one directory per test with no retention
(measured 89 dirs / ~18M in /tmp on 2026-08-20).

This runs the code_index test target with TMPDIR pointed at a fresh empty
directory and asserts no `fugu-router-code-index-*` directory survives the run.
Open defect -> expectedFailure.

    python3 scripts/test_backlog_4ffc77b5.py
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parent.parent


class CodeIndexTestsDoNotLeakTempDirs(unittest.TestCase):
    @unittest.expectedFailure  # backlog 4ffc77b5: open defect, remove when fixed
    def test_code_index_run_leaves_no_temp_repo_behind(self):
        cargo = shutil.which("cargo") or str(Path.home() / ".cargo/bin/cargo")
        tmp = tempfile.mkdtemp(prefix="b4ffc77b5.")
        self.addCleanup(shutil.rmtree, tmp, True)
        env = dict(os.environ)
        env["TMPDIR"] = tmp
        env.setdefault("CARGO_BUILD_JOBS", "2")
        p = subprocess.run(
            [cargo, "test", "-p", "fugu-router", "--test", "code_index"],
            cwd=_REPO, env=env, capture_output=True, text=True,
        )
        # Anti-vacuity: the suite really ran and passed (a build failure or a
        # zero-test run would also leave nothing behind).
        self.assertEqual(p.returncode, 0, p.stdout[-2000:] + p.stderr[-2000:])
        self.assertRegex(p.stdout, r"test result: ok\. [1-9]\d* passed")
        leaked = sorted(
            n for n in os.listdir(tmp) if n.startswith("fugu-router-code-index-")
        )
        self.assertEqual(leaked, [], "code_index tests leaked temp repos: %r" % leaked)


if __name__ == "__main__":
    unittest.main()
