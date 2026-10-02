#!/usr/bin/env python3
"""RED repro for backlog 7100bb4e.

7100bb4e: the dead-session worktree GC exists (`condukt worktree reap`, and
`condukt worktree reconcile --remove`), but nothing drives it automatically —
`crates/condukt/src/wt_reap.rs` says verbatim "Nothing runs this
automatically. It is an operator command." Since CLAUDE.md section 8 makes a
worktree per session mandatory, residue grows monotonically until a human runs
the command by hand.

Observable: no hook wiring anywhere (any plugin's hooks/hooks.json, or the
repo's .githooks/*) invokes either reclaim command. When an automatic driving
path is added, this test turns GREEN; remove the expectedFailure then.
"""

from __future__ import annotations

import glob
import os
import re
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(SCRIPTS)

RECLAIM = re.compile(r"condukt\S*\s+worktree\s+(reap\b|reconcile\b[^\n\"]*--remove)")


def hook_sources() -> list[str]:
    paths = glob.glob(os.path.join(REPO, "crates", "*", "hooks", "*.json"))
    paths += [
        p
        for p in glob.glob(os.path.join(REPO, ".githooks", "*"))
        if os.path.isfile(p)
    ]
    return sorted(paths)


class DeadSessionGcIsDriven(unittest.TestCase):
    def test_hook_sources_exist(self) -> None:
        # Control: the scan has something to look at (an empty scan would
        # "find nothing" for the wrong reason).
        srcs = hook_sources()
        self.assertTrue(
            any(p.endswith(os.path.join("condukt", "hooks", "hooks.json")) for p in srcs),
            f"condukt hooks.json not found among {srcs}",
        )

    @unittest.expectedFailure  # backlog 7100bb4e: open defect
    def test_some_hook_drives_worktree_reclaim(self) -> None:
        hits = []
        for p in hook_sources():
            with open(p, encoding="utf-8", errors="replace") as f:
                if RECLAIM.search(f.read()):
                    hits.append(p)
        self.assertTrue(
            hits,
            "no hook invokes `condukt worktree reap` / `worktree reconcile --remove`; "
            "dead-session worktrees are only reclaimed when a human runs it",
        )


if __name__ == "__main__":
    unittest.main()
