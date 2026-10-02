#!/usr/bin/env python3
"""Repro for backlog 1e04dbbf: scripts/stop-verify-worktree.py blocks a Stop on
the MAIN tree while a merge is in progress, with the generic "uncommitted
changes, move them into a worktree" text, even though CLAUDE.md 8 names
merge + conflict resolution as the ONE thing allowed on main.

The test builds a real conflicted merge in a temp repo (a main checkout, not a
worktree) and runs the hook against it.  Expected after a fix: the hook either
allows (integration in progress) or at least tells the model that a merge is in
progress.  Currently: exit 2 with the generic BLOCK text that never mentions it.
"""
import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HOOK = Path(__file__).resolve().parent / "stop-verify-worktree.py"


def sh(cwd, *args):
    return subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)


class MergeInProgressOnMain(unittest.TestCase):
    def _conflicted_main(self, tmp):
        sh(tmp, "init", "-q", "-b", "main")
        sh(tmp, "config", "user.email", "t@t")
        sh(tmp, "config", "user.name", "t")
        Path(tmp, "f.txt").write_text("base\n")
        sh(tmp, "add", "f.txt")
        sh(tmp, "commit", "-qm", "base")
        sh(tmp, "checkout", "-q", "-b", "side")
        Path(tmp, "f.txt").write_text("side\n")
        sh(tmp, "commit", "-qam", "side")
        sh(tmp, "checkout", "-q", "main")
        Path(tmp, "f.txt").write_text("main\n")
        sh(tmp, "commit", "-qam", "main")
        r = sh(tmp, "merge", "side")
        self.assertNotEqual(r.returncode, 0, "fixture: merge must conflict")
        self.assertTrue(Path(tmp, ".git", "MERGE_HEAD").exists(), "fixture: MERGE_HEAD")

    def _run(self, tmp):
        return subprocess.run(
            [sys.executable, str(HOOK)],
            input=json.dumps({"cwd": tmp, "stop_hook_active": False}),
            capture_output=True, text=True,
        )

    def test_control_plain_dirty_main_still_blocks(self):
        """Anti-vacuity: the hook must keep blocking ordinary dirt on main."""
        with tempfile.TemporaryDirectory() as tmp:
            sh(tmp, "init", "-q", "-b", "main")
            sh(tmp, "config", "user.email", "t@t")
            sh(tmp, "config", "user.name", "t")
            Path(tmp, "f.txt").write_text("a\n")
            sh(tmp, "add", "f.txt")
            sh(tmp, "commit", "-qm", "base")
            Path(tmp, "f.txt").write_text("b\n")
            r = self._run(tmp)
            self.assertEqual(r.returncode, 2, r.stderr)

    @unittest.expectedFailure  # backlog 1e04dbbf: open defect; remove when fixed
    def test_merge_in_progress_is_not_reported_as_plain_dirt(self):
        with tempfile.TemporaryDirectory() as tmp:
            self._conflicted_main(tmp)
            r = self._run(tmp)
            low = r.stderr.lower()
            # NB: the generic BLOCK text says "then merge onto main", so a bare
            # "merge" substring would pass vacuously; require the in-progress state.
            ok = r.returncode == 0 or "merge_head" in low or "in progress" in low or "in-progress" in low
            self.assertTrue(
                ok,
                f"merge in progress on main: rc={r.returncode}, stderr never mentions the merge:\n{r.stderr}",
            )


if __name__ == "__main__":
    unittest.main()
