#!/usr/bin/env python3
"""Closure-audit regression for backlog ab1328cc (audit batch b1_1, 2026-10-02).

ab1328cc: scripts/tests/canary-dryrun.sh and canary-rollback.sh died on macOS
before testing anything, on GNU-only `stat -c %Y` and `find ... -printf`
(BSD stat/find reject both). cdd4aeb4 moved the fingerprinting into
scripts/tests/lib-fingerprint.sh (python3).

This is a STATIC regression: it pins that the two scripts (and the helper they
source) contain neither construct, and — as the RED control — that the same
checker flags the pre-fix blobs at cdd4aeb4^. It does NOT run the scripts
(they drive scripts/rollout-plugins.sh in a temp sandbox).
"""
import re
import subprocess
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
SCRIPTS = [
    "scripts/tests/canary-dryrun.sh",
    "scripts/tests/canary-rollback.sh",
    "scripts/tests/lib-fingerprint.sh",
]
PRE_FIX = "cdd4aeb4^"

GNU_ONLY = [
    ("stat -c", re.compile(r"\bstat\s+(?:-[A-Za-z]*\s+)*-c\b")),
    ("find -printf", re.compile(r"\bfind\b[^\n|;]*\s-printf\b")),
]


def gnu_only_hits(text: str):
    hits = []
    for i, line in enumerate(text.splitlines(), 1):
        code = line.split("#", 1)[0] if not line.lstrip().startswith("#") else ""
        for name, rx in GNU_ONLY:
            if rx.search(code):
                hits.append((i, name, line.strip()))
    return hits


class CanaryScriptsArePortable(unittest.TestCase):
    def test_head_scripts_have_no_gnu_only_stat_or_find(self):
        for rel in SCRIPTS:
            p = ROOT / rel
            self.assertTrue(p.exists(), f"{rel} missing")
            self.assertEqual(gnu_only_hits(p.read_text()), [], f"{rel} uses a GNU-only construct")

    def test_head_scripts_parse(self):
        for rel in SCRIPTS:
            r = subprocess.run(["bash", "-n", str(ROOT / rel)], capture_output=True, text=True)
            self.assertEqual(r.returncode, 0, f"bash -n {rel}: {r.stderr}")

    def test_checker_flags_the_pre_fix_blobs(self):
        # RED control: the exact defect the item names, read from history.
        for rel in SCRIPTS[:2]:
            r = subprocess.run(
                ["git", "-C", str(ROOT), "show", f"{PRE_FIX}:{rel}"],
                capture_output=True,
                text=True,
            )
            if r.returncode != 0:
                self.skipTest(f"history for {PRE_FIX}:{rel} unavailable (shallow clone?)")
            self.assertTrue(gnu_only_hits(r.stdout), f"checker found nothing in {PRE_FIX}:{rel}")


if __name__ == "__main__":
    unittest.main()
