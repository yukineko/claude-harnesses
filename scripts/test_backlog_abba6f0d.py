#!/usr/bin/env python3
"""backlog abba6f0d: hook-reachable code spawns harness binaries by BARE name
(`Command::new("overwatch")` etc.). Hook child processes do not inherit the
plugin bin dirs on PATH, so a bare-name spawn only sees the login PATH — the
same defect overwatch 0.2.28 (e2a22f3b) fixed for `overwatch status` by moving
to `harness_core::plugin_bin::resolve`.

This test enumerates exactly the sites the item lists (file + spawned binary)
and fails while any of them still passes a bare harness binary name to
`Command::new`. (autoflow/src/main.rs's condukt spawn, also in the item, is
already gone; autoflow/src/backlog.rs's lexical `candidates.sort()` half is
measured by plugin_bin's own RED and not repeated here.)

Run:  python3 -m unittest scripts.test_backlog_abba6f0d
"""
import re
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

SITES = [
    ("crates/autoflow/src/backlog.rs", "backlog"),
    ("crates/backlog/src/store.rs", "condukt"),
    ("crates/condukt/src/claim.rs", "backlog"),
    ("crates/ctxrot/src/hooks/guard.rs", "overwatch"),
    ("crates/daily/src/main.rs", "backlog"),
    ("crates/specguard/src/forge/queue.rs", "backlog"),
    ("crates/stuckguard/src/anchor.rs", "overwatch"),
    ("crates/stuckguard/src/anchor.rs", "condukt"),
]


def bare_spawns(rel, name):
    pat = re.compile(r'Command::new\(\s*"%s"\s*\)' % re.escape(name))
    out = []
    for n, line in enumerate((REPO / rel).read_text().splitlines(), 1):
        code = line.split("//", 1)[0]
        if pat.search(code):
            out.append(f"{rel}:{n}: {line.strip()}")
    return out


class NoBareNameHarnessSpawns(unittest.TestCase):
    def test_precondition_listed_files_exist(self):
        for rel, _ in SITES:
            self.assertTrue((REPO / rel).is_file(), rel)

    def test_precondition_plugin_bin_resolver_exists(self):
        self.assertTrue((REPO / "crates/harness-core/src/plugin_bin.rs").is_file())

    @unittest.expectedFailure
    def test_listed_sites_do_not_spawn_harness_binaries_by_bare_name(self):
        """backlog abba6f0d: open defect (RED observed)."""
        hits = [h for rel, name in SITES for h in bare_spawns(rel, name)]
        self.assertEqual(
            hits, [],
            "bare-name harness spawns (not via harness_core::plugin_bin::resolve):\n"
            + "\n".join(hits))


if __name__ == "__main__":
    unittest.main()
