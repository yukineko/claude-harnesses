#!/usr/bin/env python3
"""Issue #1346: fugu-router launcher header misdescribes its `prompt` stdout.

crates/fugu-router/bin/fugu-router lines 16-17 say the `prompt` fallback
prints "stdout JSON: additionalContext ... + systemMessage", but line 59
emits

    {"systemMessage": ..., "hookSpecificOutput": {"hookEventName":
     "UserPromptSubmit", "additionalContext": ...}}

The top-level form the prose suggests is the shape Claude Code ignores for
UserPromptSubmit (see #1343), so the comment points the next author at the
wrong shape.

The test runs the launcher from a temp copy (no bundled binary, so the
fallback path runs), reads the JSON it really emits, and requires every key
of that JSON to be named in the header's `prompt` entry.

    python3 -m pytest scripts/test_issue_1346_fugu_router_header_shape.py

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

LAUNCHER = Path(__file__).resolve().parent.parent / "crates" / "fugu-router" / "bin" / "fugu-router"


def _keys(obj, out: set[str]) -> set[str]:
    if isinstance(obj, dict):
        for k, v in obj.items():
            out.add(k)
            _keys(v, out)
    return out


def _prompt_entry(text: str) -> str:
    """The header comment's `prompt` entry: from the `#   prompt` line up to
    the next entry (`#   sync`)."""
    header = []
    for line in text.splitlines():
        if not line.startswith("#"):
            break
        header.append(line)
    start = next(i for i, l in enumerate(header) if l.lstrip("# ").startswith("prompt "))
    end = next(i for i, l in enumerate(header) if i > start and l.lstrip("# ").startswith("sync "))
    return "\n".join(header[start:end])


class HeaderMatchesEmittedShape(unittest.TestCase):
    def test_prompt_entry_names_every_emitted_key(self) -> None:
        tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="issue1346.")))
        self.addCleanup(shutil.rmtree, tmp, True)
        copy = tmp / "fugu-router"
        shutil.copyfile(LAUNCHER, copy)
        env = {"HOME": str(tmp), "TMPDIR": str(tmp),
               "PATH": os.environ.get("PATH", "/usr/bin:/bin")}
        r = subprocess.run(["sh", str(copy), "prompt"], env=env, cwd=tmp,
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(r.returncode, 0, r.stderr)
        emitted = json.loads(r.stdout)
        self.assertEqual(emitted["hookSpecificOutput"]["hookEventName"], "UserPromptSubmit",
                         "anti-vacuity: the fallback emitted the nested shape")
        self.assertIn("additionalContext", emitted["hookSpecificOutput"])

        entry = _prompt_entry(LAUNCHER.read_text())
        self.assertIn("additionalContext", entry, "anti-vacuity: found the prompt entry")
        missing = sorted(k for k in _keys(emitted, set()) if k not in entry)
        self.assertEqual(missing, [],
                         "header `prompt` entry does not name the emitted shape; "
                         f"entry={entry!r} emitted={r.stdout.strip()!r}")


if __name__ == "__main__":
    unittest.main()
