#!/usr/bin/env python3
"""Issue #1235: scripts/guard-maintree-bash.py has no crash barrier.

main() catches only ValueError (payload parse) and Undetermined. Any other
exception escapes, Python exits 1, and Claude Code treats a hook exit 1 as a
non-blocking error, i.e. the tool call is allowed (fail-open, CLAUDE.md 3).
The sibling hooks deny-no-verify.py and guard-maintree-edit.py map a crash to
a refusal.

Two inputs that crash it:
  a. a deeply nested JSON payload: json.loads raises RecursionError, which is
     not a ValueError;
  b. a PostToolUse whose stored Pre snapshot has `hashes` as a list: the
     refusal branch calls changed_paths(), which does `a.get(...)` and raises
     AttributeError.

Contract asserted: both are refused with exit 2, never exit 1.

    python3 -m pytest scripts/test_issue_1235_guard_bash_crash_barrier.py

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_guard_maintree_bash_observe import SCRIPT, ObserveGuardBase, _clean_env  # noqa: E402


class CrashBarrier(ObserveGuardBase):
    def test_a_deeply_nested_payload_is_refused_not_crashed(self) -> None:
        depth = 200_000
        payload = "[" * depth + "]" * depth
        env = _clean_env()
        env["CLAUDE_PROJECT_DIR"] = str(self.main)
        env["HOME"] = str(self.tmp)
        r = subprocess.run([sys.executable, str(SCRIPT)], input=payload,
                           cwd=str(self.main), env=env, capture_output=True,
                           text=True, timeout=60)
        self.assertNotEqual(r.returncode, 1,
                            "exit 1 is allow to Claude Code (fail-open); "
                            f"stderr tail={r.stderr[-400:]!r}")
        self.assertEqual(r.returncode, 2, f"stderr tail={r.stderr[-400:]!r}")

    def test_b_snapshot_with_list_hashes_is_refused_not_crashed(self) -> None:
        tid, new = self.pre_ok("true")
        self.assertEqual(len(new), 1)
        snap_path = next(iter(new))
        snap = json.loads(snap_path.read_text())
        self.assertIsInstance(snap["hashes"], dict, "anti-vacuity: Pre wrote a dict")
        snap["hashes"] = ["tracked.txt"]
        snap_path.write_text(json.dumps(snap))
        r = self.post(tid, "true", env_override={"HOME": str(self.tmp)})
        self.assertNotEqual(r.returncode, 1,
                            "exit 1 is allow to Claude Code (fail-open); "
                            f"stderr tail={r.stderr[-400:]!r}")
        self.assertEqual(r.returncode, 2, f"stderr tail={r.stderr[-400:]!r}")


if __name__ == "__main__":
    unittest.main()
