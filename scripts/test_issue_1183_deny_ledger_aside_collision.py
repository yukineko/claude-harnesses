#!/usr/bin/env python3
"""Issue #1183: two salvages in the same second rename to the same aside name.

scripts/deny_ledger.py _read_bounded() moves a stale corrupt ledger aside to

    aside = f"{path}.corrupt-{int(now)}"

and os.rename() silently replaces an existing file of that name. A second
salvage within the same second (another hook process of the same session, or a
fresh file that went bad again) therefore overwrites the first aside, and the
original corrupt bytes, which the docstring says are "left for a human to
inspect", are lost. Observed with 8 parallel processes in the e033c406 verifier.

This test is deterministic: it calls _read_bounded() twice with the same `now`.

Contract asserted: after both salvages, the first file's corrupt bytes still
exist in some aside file in the ledger directory.

    python3 -m pytest scripts/test_issue_1183_deny_ledger_aside_collision.py

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import io
import json
import os
import shutil
import sys
import tempfile
import time
import unittest
from contextlib import redirect_stderr
from pathlib import Path
from unittest import mock

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))
import deny_ledger  # noqa: E402

SID = "issue-1183-session"


def _deny_line(target: str, root: str) -> str:
    return json.dumps({
        "v": 1, "kind": "deny", "ts": "2026-10-25T00:00:00+0000",
        "epoch": time.time() - 3600, "session_id": SID,
        "denier": "guard-maintree-edit.py", "target_abs": target, "root": root,
        "raw_target": None, "raw": "Write " + target, "reason": "main tree",
        "snapshot": {"exists": True, "is_dir": False, "mtime_ns": 1, "size": 1,
                     "status": ""},
    }) + "\n"


class AsideCollision(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = os.path.realpath(tempfile.mkdtemp(prefix="issue1183."))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        home = os.path.join(self.tmp, "home")
        tmpdir = os.path.join(self.tmp, "tmpdir")
        os.makedirs(home)
        os.makedirs(tmpdir)
        patcher = mock.patch.dict(os.environ, {"HOME": home, "TMPDIR": tmpdir})
        patcher.start()
        self.addCleanup(patcher.stop)
        self.path = deny_ledger.ledger_path(SID)
        self.assertTrue(self.path.startswith(home), "anti-vacuity: ledger under pinned HOME")
        os.makedirs(os.path.dirname(self.path))
        self.root = os.path.join(self.tmp, "main")
        self.target = os.path.join(self.root, "f.rs")

    def _write_stale(self, data: bytes, now: float) -> None:
        with open(self.path, "wb") as f:
            f.write(data)
        old = now - 25 * 60
        os.utime(self.path, (old, old))

    def _salvage(self, now: float) -> str:
        err = io.StringIO()
        with redirect_stderr(err):
            out = deny_ledger._read_bounded(self.path, now)
        self.assertEqual([e["target_abs"] for e in out], [self.target],
                         "anti-vacuity: the valid deny was salvaged")
        self.assertIn("moved it aside", err.getvalue())
        return err.getvalue()

    def test_two_salvages_in_the_same_second_keep_the_first_corrupt_original(self) -> None:
        now = float(int(time.time())) + 0.25
        first = b"{garbage-ONE\n" + _deny_line(self.target, self.root).encode()
        self._write_stale(first, now)
        self._salvage(now)
        # The fresh salvaged file goes bad again; a second salvage runs
        # within the same second.
        with open(self.path, "ab") as f:
            f.write(b"{garbage-TWO\n")
        os.utime(self.path, (now - 25 * 60, now - 25 * 60))
        self._salvage(now + 0.5)

        d = os.path.dirname(self.path)
        asides = sorted(n for n in os.listdir(d) if ".corrupt-" in n)
        contents = {n: Path(d, n).read_bytes() for n in asides}
        self.assertTrue(
            any(b"{garbage-ONE" in c for c in contents.values()),
            "the first corrupt original was overwritten by the second salvage; "
            f"asides={ {n: c[:40] for n, c in contents.items()} }")


if __name__ == "__main__":
    unittest.main()
