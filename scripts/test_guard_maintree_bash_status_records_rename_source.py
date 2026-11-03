#!/usr/bin/env python3
"""RED repro: guard-maintree-bash status_records() misreads a rename source field.

Observed 2026-11-03 while verifying condukt run run-20261103-233558-54331 task
gh1512-maintree-merge (worker note, file scripts/guard-maintree-bash.py
status_records): every NUL-separated field whose first character is one of
1 2 u ? ! is treated as a record of its own. In `git status --porcelain=v2 -z`
a rename/copy record ("2 ...") is followed by a SEPARATE field holding the
source path, so a source path that starts with one of those characters is
parsed as a record:
  * "1src.txt" -> status_paths("1src.txt\\0") -> rec.split(" ", 8)[8] raises
    IndexError; the hook's crash handler turns that into exit 2 (a refusal of
    a command that may have been a legitimate integration);
  * "? x" style sources would add a phantom path entry.
status_paths() itself consumes the source field correctly; only
status_records() re-parses fields one at a time.

Two readings: (a) defect, status_records must skip the source field exactly
as status_paths does; (b) intended over-refusal. (b) is unlikely: the inline
comment in status_records says "a rename record's source field is a separate
entry" and it already tries to skip it via `if p:`. This test pins (a).
"""
from __future__ import annotations

import importlib.util
import sys
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "guard-maintree-bash.py"


def _load():
    spec = importlib.util.spec_from_file_location("guard_maintree_bash", SCRIPT)
    mod = importlib.util.module_from_spec(spec)
    assert spec.loader is not None
    spec.loader.exec_module(mod)
    return mod


OID = "0" * 40
RENAME = f"2 R. N... 100644 100644 100644 {OID} {OID} R100 new.txt"


class RenameSourceField(unittest.TestCase):
    def setUp(self):
        self.g = _load()

    def check(self, source: str):
        porcelain = RENAME + "\0" + source + "\0"
        # status_paths is the reference parse of the same output.
        self.assertEqual(self.g.status_paths(porcelain), ["new.txt", source])
        try:
            recs = self.g.status_records(porcelain)
        except Exception as exc:  # noqa: BLE001
            self.fail(f"status_records crashed on rename source {source!r}: {type(exc).__name__}: {exc}")
        self.assertEqual(
            recs.get("new.txt"), RENAME,
            f"rename destination must map to its record; got {recs!r}")
        self.assertNotIn(
            source[2:], [p for p in recs if p != "new.txt" and p != source],
            f"source field must not become a phantom path; got {recs!r}")
        for p, rec in recs.items():
            self.assertNotEqual(rec, source, f"source field {source!r} parsed as a record: {recs!r}")

    def test_source_starting_with_1(self):
        self.check("1src.txt")

    def test_source_starting_with_question_mark(self):
        self.check("? weird.txt")

    def test_plain_source(self):
        self.check("old.txt")


if __name__ == "__main__":
    unittest.main()
