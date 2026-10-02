#!/usr/bin/env python3
"""backlog fbf93b81: rebuild-plugins.sh overwrites LIVE plugin-cache binaries
in place (`cp -f "$src" "$binfile"` / `cp -f "$src" "$hostbin"`), so a hook
that launches during the copy can see a partially written / not-yet-executable
binary (the launcher branches on `[ -x "$binary" ]`). The measured symptom was
SessionStart `overwatch status` reporting "no bundled binary" mid-rollout.

The item's own discriminator is the code shape: "cp なら (a) が実在の窓である"
— an in-place cp leaves the window, a same-FS temp write + rename closes it.
This test reads scripts/rebuild-plugins.sh and fails while any live-cache
binary destination is written by an in-place `cp -f "$src" <dest>`.
(scripts/rollout-plugins.sh's `cp -a "$src/." "$dst/"` directory copy, also
in the audit row, is not covered here.)

Run:  python3 -m unittest scripts.test_backlog_fbf93b81
"""
import re
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
REBUILD = SCRIPTS / "rebuild-plugins.sh"

# Destinations that are the live cache the running harness execs.
LIVE_DESTS = ("$binfile", "$hostbin")


class LiveCacheBinaryCopyIsAtomic(unittest.TestCase):
    def test_precondition_live_dest_vars_still_exist(self):
        src = REBUILD.read_text()
        for d in LIVE_DESTS:
            self.assertIn(d, src, f"{d} vanished from rebuild-plugins.sh; update this test")

    @unittest.expectedFailure
    def test_no_in_place_cp_onto_a_live_cache_binary(self):
        """backlog fbf93b81: open defect (RED observed)."""
        hits = []
        for n, line in enumerate(REBUILD.read_text().splitlines(), 1):
            code = line.split("#", 1)[0]
            for d in LIVE_DESTS:
                if re.search(r'\bcp\s+-f\s+"\$src"\s+"%s"' % re.escape(d), code):
                    hits.append(f"rebuild-plugins.sh:{n}: {line.strip()}")
        self.assertEqual(
            hits, [],
            "live cache binaries are overwritten in place (no temp+rename):\n"
            + "\n".join(hits))


if __name__ == "__main__":
    unittest.main()
