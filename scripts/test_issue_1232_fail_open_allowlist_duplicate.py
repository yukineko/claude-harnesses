#!/usr/bin/env python3
"""Issue #1232: a check-fail-open ALLOWLIST entry exempts every identical line.

scripts/check-fail-open.py _allowlisted() matches an entry on path + rule +
`needle in line`, so it exempts EVERY line in that file containing the needle,
not the one reviewed line. A second, unreviewed copy of the reviewed line in
the same file is dropped silently: scan_shell() reports 2 raw hits but
scan_file() returns [] and the counted total does not grow.

Uses the one real ALLOWLIST entry (scripts/test-changed-crates.sh,
"git ls-files 2>/dev/null") against a copy of that file in a temp REPO.

Control: the file with the reviewed line once is clean.
Contract asserted: with that line duplicated, scan_file() still reports the
unreviewed copy (at least one finding).

    python3 -m pytest scripts/test_issue_1232_fail_open_allowlist_duplicate.py

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import importlib.util
import os
import shutil
import tempfile
import unittest
from pathlib import Path
from unittest import mock

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location("check_fail_open", _HERE / "check-fail-open.py")
fo = importlib.util.module_from_spec(_SPEC)
exec(  # noqa: S102  (source text, not a cached .pyc; see test_check_fail_open.py)
    compile(_SPEC.loader.get_source(_SPEC.name), _SPEC.origin, "exec", dont_inherit=True),
    fo.__dict__,
)

REL = "scripts/test-changed-crates.sh"
NEEDLE = "git ls-files 2>/dev/null"


class AllowlistDuplicateLine(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="issue1232.")))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        patcher = mock.patch.dict(os.environ, {"HOME": str(self.tmp / "home"),
                                               "TMPDIR": str(self.tmp)})
        patcher.start()
        self.addCleanup(patcher.stop)
        entries = [e for e in fo.ALLOWLIST if e["path"] == REL and e["needle"] == NEEDLE]
        self.assertEqual(len(entries), 1, "anti-vacuity: the real allowlist entry exists")
        self.rule = entries[0]["pattern"]
        self.src = (_HERE / "test-changed-crates.sh").read_text().splitlines(keepends=True)
        self.idx = next(i for i, l in enumerate(self.src) if NEEDLE in l)
        self.target = self.tmp / REL
        self.target.parent.mkdir(parents=True)
        p = mock.patch.object(fo, "REPO", self.tmp)
        p.start()
        self.addCleanup(p.stop)

    def _write(self, lines: list[str]) -> None:
        self.target.write_text("".join(lines))

    def _raw_hits(self) -> list:
        return [h for h in fo.scan_shell(self.target.read_text().splitlines())
                if h[2] == self.rule and NEEDLE in h[1]]

    def test_control_single_reviewed_line_is_exempt(self) -> None:
        self._write(self.src)
        self.assertEqual(len(self._raw_hits()), 1, "anti-vacuity: one raw hit")
        self.assertEqual(fo.scan_file(self.target), [])

    def test_duplicated_reviewed_line_is_still_reported(self) -> None:
        dup = self.src[: self.idx + 1] + [self.src[self.idx]] + self.src[self.idx + 1:]
        self._write(dup)
        self.assertEqual(len(self._raw_hits()), 2, "anti-vacuity: two raw hits")
        found = fo.scan_file(self.target)
        self.assertGreaterEqual(
            len(found), 1,
            "the unreviewed duplicate was exempted by the reviewed line's needle; "
            f"scan_file={found!r}")


if __name__ == "__main__":
    unittest.main()
