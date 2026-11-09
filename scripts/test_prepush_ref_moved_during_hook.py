#!/usr/bin/env python3
"""RED repro: .githooks/pre-push certifies a sha that is not the one pushed.

Observed 2026-10-25 on this repo. Three concurrent `git push origin main` runs
from one checkout (origin is https). Push A started at 09:04:45 with main at
2f8e4209. Its pre-push hook checked 2f8e4209 and the client printed
`c2f59694..2f8e4209  main -> main`. GitHub's activity log for refs/heads/main
records that push, at 00:09:54Z, as `c2f59694..4d3dd426`. During A's hook, main
had moved to 0241c62f (09:07:14) and then 4d3dd426 (09:09:08). The other two
pushes then failed with `is at 4d3dd426... but expected c2f59694...`.

Why: over a remote helper transport (http/https), git hands the helper the
refspec BY NAME (`push refs/heads/main:refs/heads/main`). The helper re-reads
refs/heads/main after the pre-push hook returns. So when the local ref moves
while the hook runs, the remote gets a commit the hook never saw. The client
still prints the range for the sha it certified. file:// pushes use the sha
and are not affected. A local smart-HTTP reproduction confirmed both results.

The hook's own comment says its checks "must judge WHAT IS BEING PUSHED". The
gate-bypass ledger, cargo check, compile-fail and specguard checks judged
2f8e4209. 0241c62f and 4d3dd426 went out without passing them.

Property pinned: if a pushed local ref no longer names the sha the hook was
given by the time the hook finishes, the hook cannot vouch for what git will
send, so it must block. Cannot determine resolves to block (CLAUDE.md 3).
Fixed (backlog d222b3e3): the hook now re-checks each pushed ref before its
final exit 0, so this test is expected to pass. The RED was observed at
13e1bd83 with
`python3 -m pytest --runxfail scripts/test_prepush_ref_moved_during_hook.py`.
"""

from __future__ import annotations

import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
import json  # noqa: E402

from test_prepush_hook import CRATE_DIR, CRATE_PKG, HookHarness  # noqa: E402

# A cargo stand-in that, like a concurrent `git merge` in another session,
# moves refs/heads/main while the hook is running. MOVE_TO is set per test.
# Since the 2026-11-08 ruling the hook asks `cargo metadata` for package names
# before `cargo check`, so the stand-in answers that (unchanged from the
# harness's own stub) and moves main only from `cargo check` — the point where
# the old stand-in moved it too.
_CARGO_MOVES_MAIN = """#!/bin/sh
if [ "${{1:-}}" = "metadata" ]; then
    cat <<'JSON'
{metadata}
JSON
    exit 0
fi
git update-ref refs/heads/main {move_to}
exit 0
"""


def _harness_with_moving_main(add_cleanup):
    # crate=True: cargo runs only when the pushed range touches a crate
    # (2026-11-08 ruling); without it the stand-in below is never invoked.
    h = HookHarness(gate_bypass_exit=0, crate=True)
    add_cleanup(h.cleanup)
    git = ["git", "-C", str(h.root)]
    tree = subprocess.run(
        git + ["rev-parse", h.local_sha + "^{tree}"],
        check=True,
        capture_output=True,
        text=True,
    ).stdout.strip()
    # A commit the hook is never given on stdin: main moves to it mid-hook.
    moved = subprocess.run(
        git + ["commit-tree", tree, "-p", h.local_sha, "-m", "merged by another session"],
        check=True,
        capture_output=True,
        text=True,
        env={"GIT_AUTHOR_NAME": "t", "GIT_AUTHOR_EMAIL": "t@x", "GIT_COMMITTER_NAME": "t",
             "GIT_COMMITTER_EMAIL": "t@x", "PATH": "/usr/bin:/bin"},
    ).stdout.strip()
    cargo = h.bindir / "cargo"
    metadata = json.dumps({
        "packages": [{"name": CRATE_PKG, "version": "0.1.0", "id": CRATE_PKG + " 0.1.0",
                      "manifest_path": str(h.root / CRATE_DIR / "Cargo.toml")}],
        "workspace_members": [CRATE_PKG + " 0.1.0"],
        "target_directory": str(h.root / "target"),
        "workspace_root": str(h.root),
        "version": 1,
    })
    cargo.write_text(_CARGO_MOVES_MAIN.format(move_to=moved, metadata=metadata))
    cargo.chmod(0o755)
    return h, moved


class RefMovedDuringHookTests(unittest.TestCase):
    def test_control_unmoved_ref_is_allowed(self):
        """Anti-vacuity: the same harness with main left alone is ALLOWED, so the
        block asserted below is about the moved ref and not a harness defect."""
        h = HookHarness(gate_bypass_exit=0, crate=True)
        self.addCleanup(h.cleanup)
        proc = h.run()
        assert proc.returncode == 0, proc.stderr
        # The same check the moving stand-in hooks into really runs here.
        assert h.cargo_checks(), "cargo check never ran\n" + proc.stderr

    def test_ref_moved_during_hook_blocks(self):
        h, moved = _harness_with_moving_main(self.addCleanup)
        proc = h.run()
        now = subprocess.run(
            ["git", "-C", str(h.root), "rev-parse", "refs/heads/main"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()
        # Precondition: the stand-in really moved main away from the certified sha.
        assert now == moved != h.local_sha
        assert proc.returncode != 0, (
            "pre-push exited 0 although refs/heads/main moved from the certified "
            "%s to %s during the hook; over https git sends %s unchecked\n--- stderr ---\n%s"
            % (h.local_sha, moved, moved, proc.stderr)
        )


if __name__ == "__main__":
    unittest.main()
