#!/usr/bin/env python3
"""Production paths of the session-age hold (backlog 18fe626f) that
test_prune_session_age_hold.py does not reach.

That file drives the hold through the PLUGIN_CACHE_PROC_LIST_PROBE /
PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE test seams.  This file covers what runs
when neither seam is set, plus the override's own validation:

  1. default process list: `ps -A -o pid=,etime=,comm=` (a stub `ps` on PATH;
     no real process is ever read).  etime formats MM:SS, HH:MM:SS,
     D-HH:MM:SS; older claude holds, newer does not; ps failure / garbage /
     empty output are undetermined (nothing removed, exit non-zero); comm is
     matched by basename (`/usr/local/bin/claude` yes, `Claude` no).
  2. superseded-at is the `lastUpdated` of the plugin's installed_plugins.json
     entry.  Missing / unparseable / plugin absent => the dir is KEPT and
     `KEPT` is named on stderr.  `installedAt` is never a fallback.
  3. PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE malformed / non-object / non-numeric
     => the whole run is undetermined (nothing removed, exit non-zero).

Every test uses a temp HOME / cache / registry and runs the real pruner as a
subprocess.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

_HERE = os.path.dirname(os.path.abspath(__file__))
_PRUNE = os.path.join(_HERE, "prune-plugin-cache.py")

MIN = 60
HOUR = 3600
DAY = 86400


def _iso(epoch):
    return time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(epoch))


def _mk_version(cache, plugin, version):
    d = Path(cache) / plugin / version
    d.mkdir(parents=True, exist_ok=True)
    (d / "payload.txt").write_text(version, encoding="utf-8")
    return d


class Base(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp = Path(self._tmp.name)
        self.crates = self.tmp / "crates"
        self.cache = self.tmp / "cache"
        self.crates.mkdir()
        self.cache.mkdir()
        pj = self.crates / "backlog" / ".claude-plugin" / "plugin.json"
        pj.parent.mkdir(parents=True)
        pj.write_text('{"name": "backlog", "version": "0.3.22"}', encoding="utf-8")
        self.cur = _mk_version(self.cache, "backlog", "0.3.22")
        self.old = _mk_version(self.cache, "backlog", "0.3.21")
        self.registry = self.tmp / "installed_plugins.json"
        self.settings = self.tmp / "settings.json"
        self.settings.write_text("{}", encoding="utf-8")
        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self.now = int(time.time())

    # -- fixtures --------------------------------------------------------
    def write_registry(self, **entry_fields):
        ent = {"installPath": str(self.cur)}
        ent.update(entry_fields)
        self.registry.write_text(
            json.dumps({"plugins": {"backlog@yukineko": [ent]}}), encoding="utf-8"
        )

    def superseded_ago(self, seconds):
        """Registry says backlog was repointed `seconds` ago."""
        self.write_registry(lastUpdated=_iso(self.now - seconds))

    def stub_ps(self, stdout="", exit_code=0):
        f = self.tmp / "ps-output.txt"
        f.write_text(stdout, encoding="utf-8")
        stub = self.bin / "ps"
        stub.write_text(f"#!/bin/sh\ncat '{f}'\nexit {exit_code}\n", encoding="utf-8")
        stub.chmod(0o755)

    # -- runner ----------------------------------------------------------
    def run_prune(self, extra_env=None, dry_run=False):
        env = {
            "PATH": f"{self.bin}:/usr/bin:/bin",
            "HOME": str(self.tmp / "home"),
            "CLAUDE_PLUGIN_CACHE": str(self.cache),
            "CLAUDE_PLUGIN_REGISTRY": str(self.registry),
            "CLAUDE_SETTINGS_JSON": str(self.settings),
        }
        env.update(extra_env or {})
        cmd = [
            sys.executable, _PRUNE,
            "--repo", str(self.tmp),
            "--cache", str(self.cache),
            "--registry", str(self.registry),
        ]
        if dry_run:
            cmd.append("--dry-run")
        p = subprocess.run(cmd, env=env, capture_output=True, text=True, timeout=60)
        return p.stdout, p.stderr, p.returncode

    def assert_kept_and_failed(self, res):
        out, err, rc = res
        self.assertTrue(self.old.is_dir(), f"must keep. out={out!r} err={err!r}")
        self.assertTrue(self.cur.is_dir())
        self.assertNotEqual(rc, 0, f"undetermined must exit non-zero. out={out!r} err={err!r}")

    def assert_pruned_clean(self, res):
        out, err, rc = res
        self.assertFalse(self.old.exists(), f"must prune. out={out!r} err={err!r}")
        self.assertIn("pruned backlog/0.3.21", out)
        self.assertEqual(rc, 0, f"out={out!r} err={err!r}")
        self.assertTrue(self.cur.is_dir())

    def assert_held(self, res):
        out, err, rc = res
        self.assertTrue(self.old.is_dir(), f"held dir removed. out={out!r} err={err!r}")
        self.assertTrue(self.cur.is_dir())


SELF = "4242 00:01 python3\n"


class DefaultPsProbe(Base):
    """Group 1: no PLUGIN_CACHE_PROC_LIST_PROBE; `ps` is a stub on PATH."""

    def _fmt_case(self, etime, held_ago, free_ago):
        # Supersession placed after the claude started => held; before => free.
        self.stub_ps(SELF + f"9001 {etime} claude\n")
        self.superseded_ago(held_ago)
        self.assert_held(self.run_prune())
        self.superseded_ago(free_ago)
        self.assert_pruned_clean(self.run_prune())

    def test_etime_mm_ss(self):
        # 10:00 = 600s old.
        self._fmt_case("10:00", 5 * MIN, 20 * MIN)

    def test_etime_hh_mm_ss(self):
        # 03:00:00 = 3h old.
        self._fmt_case("03:00:00", 2 * HOUR, 4 * HOUR)

    def test_etime_d_hh_mm_ss(self):
        # 2-03:00:00 = 51h old.
        self._fmt_case("2-03:00:00", 1 * DAY, 3 * DAY)

    def test_older_claude_holds_even_next_to_newer_one(self):
        self.stub_ps(SELF + "9001 00:30 claude\n9002 2-00:00:00 claude\n")
        self.superseded_ago(1 * HOUR)
        self.assert_held(self.run_prune())

    def test_only_newer_claude_does_not_hold(self):
        self.stub_ps(SELF + "9001 00:30 claude\n")
        self.superseded_ago(1 * HOUR)
        self.assert_pruned_clean(self.run_prune())

    def test_comm_full_path_matches_by_basename(self):
        self.stub_ps(SELF + "9001 03:00:00 /usr/local/bin/claude\n")
        self.superseded_ago(1 * HOUR)
        self.assert_held(self.run_prune())

    def test_desktop_Claude_does_not_hold(self):
        self.stub_ps(SELF + "9001 03:00:00 Claude\n")
        self.superseded_ago(1 * HOUR)
        self.assert_pruned_clean(self.run_prune())

    def test_other_names_do_not_hold(self):
        self.stub_ps(SELF + "9001 03:00:00 zsh\n9002 03:00:00 node\n")
        self.superseded_ago(1 * HOUR)
        self.assert_pruned_clean(self.run_prune())

    def test_ps_nonzero_exit_is_undetermined(self):
        # Parseable "nobody holds" output, then failure: only an exit-status
        # check keeps the dir.
        self.stub_ps(SELF + "9001 00:05 claude\n", exit_code=3)
        self.superseded_ago(1 * HOUR)
        self.assert_kept_and_failed(self.run_prune())

    def test_garbage_line_is_undetermined(self):
        self.stub_ps(SELF + "this is not a process line\n9001 00:05 claude\n")
        self.superseded_ago(1 * HOUR)
        self.assert_kept_and_failed(self.run_prune())

    def test_bad_etime_is_undetermined(self):
        self.stub_ps(SELF + "9001 notatime claude\n")
        self.superseded_ago(1 * HOUR)
        self.assert_kept_and_failed(self.run_prune())

    def test_empty_output_is_undetermined(self):
        self.stub_ps("", exit_code=0)
        self.superseded_ago(1 * HOUR)
        self.assert_kept_and_failed(self.run_prune())

    def test_dry_run_with_undetermined_exits_nonzero_and_removes_nothing(self):
        self.stub_ps("", exit_code=0)
        self.superseded_ago(1 * HOUR)
        out, err, rc = self.run_prune(dry_run=True)
        self.assertNotEqual(rc, 0, f"out={out!r} err={err!r}")
        self.assertNotIn("would remove backlog/0.3.21", out)
        self.assertTrue(self.old.is_dir())

    def test_dry_run_determined_free_exits_zero_and_lists(self):
        self.stub_ps(SELF + "9001 00:30 claude\n")
        self.superseded_ago(1 * HOUR)
        out, err, rc = self.run_prune(dry_run=True)
        self.assertEqual(rc, 0, f"out={out!r} err={err!r}")
        self.assertIn("would remove backlog/0.3.21", out)
        self.assertTrue(self.old.is_dir(), "dry-run deletes nothing")


class SupersededAtDerivation(Base):
    """Group 2: superseded-at = registry `lastUpdated` of the plugin's entry."""

    def setUp(self):
        super().setUp()
        # One claude that started 10 minutes ago.
        self.stub_ps(SELF + "9001 10:00 claude\n")

    def test_lastupdated_older_than_claude_frees(self):
        self.superseded_ago(1 * HOUR)
        self.assert_pruned_clean(self.run_prune())

    def test_lastupdated_newer_than_claude_holds(self):
        self.superseded_ago(1 * MIN)
        self.assert_held(self.run_prune())

    def test_lastupdated_used_even_when_installedAt_disagrees(self):
        self.write_registry(
            installedAt=_iso(self.now - 365 * DAY), lastUpdated=_iso(self.now - 1 * MIN)
        )
        self.assert_held(self.run_prune())

    def _assert_kept_named(self, res):
        out, err, rc = res
        self.assert_kept_and_failed(res)
        self.assertIn("KEPT", err, f"KEPT must be named on stderr. err={err!r}")
        self.assertIn("backlog/0.3.21", err)

    def test_missing_lastupdated_keeps_and_names_it(self):
        self.write_registry()
        self._assert_kept_named(self.run_prune())

    def test_unparseable_lastupdated_keeps_and_names_it(self):
        self.write_registry(lastUpdated="yesterday-ish")
        self._assert_kept_named(self.run_prune())

    def test_non_string_lastupdated_keeps_and_names_it(self):
        self.write_registry(lastUpdated=12345)
        self._assert_kept_named(self.run_prune())

    def test_plugin_absent_from_registry_keeps_and_names_it(self):
        other = _mk_version(self.cache, "other", "1.0.0")
        self.registry.write_text(
            json.dumps(
                {
                    "plugins": {
                        "other@yukineko": [
                            {"installPath": str(other), "lastUpdated": _iso(self.now - DAY)}
                        ]
                    }
                }
            ),
            encoding="utf-8",
        )
        self._assert_kept_named(self.run_prune())

    def test_installedAt_is_not_a_fallback(self):
        # Only installedAt, a year ago; the one claude (10 min old) is YOUNGER
        # than it. A fallback reads "superseded a year ago, every claude is
        # newer" and prunes. The dir must be kept.
        self.write_registry(installedAt=_iso(self.now - 365 * DAY))
        self._assert_kept_named(self.run_prune())

    def test_installedAt_is_not_a_fallback_for_unparseable_lastupdated(self):
        self.write_registry(installedAt=_iso(self.now - 365 * DAY), lastUpdated="garbage")
        self._assert_kept_named(self.run_prune())


class OverrideValidation(Base):
    """Group 3: PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE must be valid or the whole
    run is undetermined. Registry and process list are valid and would
    otherwise let 0.3.21 be pruned."""

    def setUp(self):
        super().setUp()
        self.superseded_ago(1 * HOUR)
        self.stub_ps(SELF + "9001 00:30 claude\n")

    def _check(self, raw):
        self.assert_kept_and_failed(
            self.run_prune({"PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE": raw})
        )

    def test_control_valid_override_prunes(self):
        raw = json.dumps({"backlog/0.3.21": self.now - 2 * HOUR})
        self.assert_pruned_clean(
            self.run_prune({"PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE": raw})
        )

    def test_malformed_json(self):
        self._check("{not json")

    def test_non_object_list(self):
        self._check("[]")

    def test_non_object_scalar(self):
        self._check("5")

    def test_non_numeric_value(self):
        self._check(json.dumps({"backlog/0.3.21": "abc"}))

    def test_null_value(self):
        self._check(json.dumps({"backlog/0.3.21": None}))

    def test_bad_key_shape(self):
        self._check(json.dumps({"backlog": 1000}))

    def test_undetermined_override_dry_run_exits_nonzero(self):
        out, err, rc = self.run_prune(
            {"PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE": "{not json"}, dry_run=True
        )
        self.assertNotEqual(rc, 0, f"out={out!r} err={err!r}")
        self.assertTrue(self.old.is_dir())


if __name__ == "__main__":
    unittest.main()
