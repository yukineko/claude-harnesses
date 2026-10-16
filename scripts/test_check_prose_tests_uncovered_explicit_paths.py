#!/usr/bin/env python3
"""check-prose-tests.py given explicit FILE arguments, none of which is a
covered path, exits 0 silently.

main() filters the explicit files with is_covered() and, if nothing survives,
checks zero files and returns 0 ("no findings"). The module's own contract is
that "`--all` found nothing to scan" is exit 2 (undetermined), and the explicit
FILE form is documented as "check every unit" of the named files. A caller
that names files and gets nothing checked -- because of a typo, a path the
gate does not cover, or a rename -- reads the silent 0 as "checked and clean".

Claimed property: when every explicit path is outside the covered set, nothing
was checked, which is undetermined: exit 2 (fail-closed), not 0.

  control: `--all` over an empty repo exits 2 (the documented empty-scan rule)
  repro:   `--repo <empty> some/uncovered.txt` must exit 2; today it exits 0

RED repro:  python3 -m pytest --runxfail scripts/test_check_prose_tests_uncovered_explicit_paths.py
"""
from __future__ import annotations

import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "check-prose-tests.py"


def run(*args: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(SCRIPT), *args], capture_output=True, text=True
    )


class UncoveredExplicitPaths(unittest.TestCase):
    def test_control_all_over_empty_repo_is_undetermined(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            r = run("--repo", t, "--all")
            self.assertEqual(r.returncode, 2, r.stdout + r.stderr)

    @unittest.expectedFailure
    def test_explicit_uncovered_path_is_undetermined_not_clean(self) -> None:
        with tempfile.TemporaryDirectory() as t:
            r = run("--repo", t, "some/uncovered.txt")
            self.assertEqual(
                r.returncode,
                2,
                "explicit path outside the covered set checked NOTHING yet "
                f"exited {r.returncode}\nstdout={r.stdout!r}\nstderr={r.stderr!r}",
            )


if __name__ == "__main__":
    unittest.main()
