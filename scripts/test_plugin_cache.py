#!/usr/bin/env python3
"""Tests for the shared cache facts: staleness and live-session holding.

These pin the rule BOTH consumers obey — the gate that reports superseded
version dirs and the pruner that deletes them. The dangerous direction here is
not a missed report, it is deleting a directory a running session is executing
from, so the "held" cases carry as much weight as the "removable" one.
"""
import importlib.util
import os
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = os.path.dirname(os.path.abspath(__file__))
_spec = importlib.util.spec_from_file_location(
    "plugin_cache", os.path.join(_HERE, "plugin_cache.py")
)
pc = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(pc)


def _plugin(crates, name, version):
    pj = Path(crates) / name / ".claude-plugin" / "plugin.json"
    pj.parent.mkdir(parents=True, exist_ok=True)
    pj.write_text(f'{{"name": "{name}", "version": "{version}"}}', encoding="utf-8")


def _cached(cache, name, version, marker_pids=(), tmp_markers=()):
    d = Path(cache) / name / version
    d.mkdir(parents=True, exist_ok=True)
    (d / "payload.txt").write_text(version, encoding="utf-8")
    if marker_pids or tmp_markers:
        m = d / pc.IN_USE_DIR
        m.mkdir(exist_ok=True)
        for p in marker_pids:
            (m / str(p)).write_text("x", encoding="utf-8")
        for t in tmp_markers:
            (m / t).write_text("x", encoding="utf-8")
    return d


class Liveness(unittest.TestCase):
    def test_own_pid_is_alive(self):
        self.assertIs(pc.pid_alive(os.getpid()), True)

    def test_pid_1_is_alive_even_though_not_ours(self):
        """EPERM means the process EXISTS. Reading it as dead would let the
        pruner delete a version dir held by another user's running session."""
        self.assertIs(pc.pid_alive(1), True)

    def test_absent_pid_is_dead(self):
        # Find a pid that is genuinely gone rather than assuming a magic number.
        dead = None
        for candidate in range(4194300, 4194200, -1):
            if pc.pid_alive(candidate) is False:
                dead = candidate
                break
        self.assertIsNotNone(dead, "no dead pid found to test with")
        self.assertIs(pc.pid_alive(dead), False)


class Staleness(unittest.TestCase):
    def _fixture(self, tmp):
        crates, cache = Path(tmp) / "crates", Path(tmp) / "cache"
        crates.mkdir()
        cache.mkdir()
        return crates, cache

    def test_superseded_dir_with_no_markers_is_removable(self):
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "1.0.0")
            _cached(cache, "alpha", "2.0.0")
            cur, probs = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(probs + sprobs, [])
            self.assertEqual([s.version for s in stale], ["1.0.0"])
            self.assertTrue(stale[0].removable)

    def test_current_version_is_never_stale(self):
        """Control arm: without this, a rule that called EVERY dir stale would
        satisfy the test above and hand the pruner the live directory."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "2.0.0")
            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur)
            self.assertEqual(stale, [])

    def test_dir_held_by_live_pid_is_not_removable(self):
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "1.0.0", marker_pids=[os.getpid()])
            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur)
            self.assertEqual(len(stale), 1)
            self.assertFalse(stale[0].removable)
            self.assertIn(os.getpid(), stale[0].holders.live_pids)

    def test_dead_markers_do_not_hold_a_dir(self):
        """The measured real-world state: 64 accumulated markers, all dead.
        If mere presence of .in_use held a dir, nothing would ever be pruned."""
        dead = next(
            c for c in range(4194300, 4194200, -1) if pc.pid_alive(c) is False
        )
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            _cached(cache, "alpha", "1.0.0", marker_pids=[dead],
                    tmp_markers=["19808.tmp.3e6c10da"])
            cur, _ = pc.source_versions(str(crates))
            stale, _ = pc.scan(str(cache), cur)
            self.assertTrue(stale[0].removable)

    def test_plugin_absent_from_source_yields_no_stale_dirs(self):
        """Not knowing which version is current must not mean 'all of them are
        stale' — that aims the pruner at the live dir."""
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _cached(cache, "ghost", "1.0.0")
            _cached(cache, "ghost", "2.0.0")
            cur, _ = pc.source_versions(str(crates))
            stale, sprobs = pc.scan(str(cache), cur)
            self.assertEqual(stale, [])
            self.assertTrue(any("ghost" in p for p in sprobs))

    def test_unreadable_marker_dir_is_undetermined_and_kept(self):
        if os.geteuid() == 0:
            self.skipTest("root ignores directory permissions")
        with tempfile.TemporaryDirectory() as tmp:
            crates, cache = self._fixture(tmp)
            _plugin(crates, "alpha", "2.0.0")
            d = _cached(cache, "alpha", "1.0.0", marker_pids=[os.getpid()])
            marker = d / pc.IN_USE_DIR
            os.chmod(marker, 0o000)
            try:
                cur, _ = pc.source_versions(str(crates))
                stale, _ = pc.scan(str(cache), cur)
                self.assertEqual(len(stale), 1)
                self.assertIsNotNone(stale[0].holders.undetermined)
                self.assertFalse(
                    stale[0].removable,
                    "an uninspectable dir must be kept, not deleted",
                )
            finally:
                os.chmod(marker, 0o700)


if __name__ == "__main__":
    unittest.main()
