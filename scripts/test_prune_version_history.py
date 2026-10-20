#!/usr/bin/env python3
"""v2 exact-interval session hold for prune-plugin-cache.py (backlog 18fe626f).

Why v2 exists: v1 (825f3381) used the plugin's registry `lastUpdated` -- the
LATEST repoint -- as superseded-at for EVERY old version dir. With continuous
rollouts a session older than the newest rollout then held every old version
forever (over-holding). User ruling 2026-10-03: over-holding is not acceptable;
Windows support is not needed (nothing here tests or mentions it).

Rule under test: a live `claude` that started at S holds version dir V iff
    activated_at(V) <= S < superseded_at(V)
i.e. V was the registry's current version when S started.

SEAMS / FORMAT the implementer must match (taken from the v2 spec)
===================================================================
1. Ledger file, one per plugin, written by the rollout repoint step:
       <cache>/<plugin>/.version-history.jsonl
   (<cache> is the marketplace-level cache dir, the one passed as --cache and
   as CLAUDE_PLUGIN_CACHE). One JSON object per line:
       {"version": "<new>", "activated_at": <int epoch s>, "previous": "<old>"|null}
   activated_at(V)  = activated_at of the line whose  version  == V
   superseded_at(V) = activated_at of the line whose  previous == V
   Tests write this file DIRECTLY for H1-H4/H6/H7 and drive the real rollout
   script for H5.
2. PLUGIN_CACHE_PROC_LIST_PROBE: unchanged from v1 (`<pid> <start_epoch> <comm>`
   per line). PLUGIN_CACHE_SUPERSEDED_AT_OVERRIDE is NOT set by any test here.
3. Legacy dir (superseded, no ledger line names it): hold only if some live
   claude started before the registry `lastUpdated` of the plugin; then the
   dir is kept and stderr carries a line starting `KEPT <plugin>/<ver>:` that
   contains `no version history`. Exit code in that case is NOT asserted (the
   spec does not pin it).
4. Corrupt/unreadable ledger => every superseded dir of THAT plugin kept and
   exit non-zero, even when no claude is alive at all.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
PRUNE = HERE / "prune-plugin-cache.py"
ROLLOUT = HERE / "rollout-plugins.sh"

SELF_LINE = "424200 1 python3"
LEDGER = ".version-history.jsonl"
PLUGIN = "backlog"


def _iso(epoch):
    return time.strftime("%Y-%m-%dT%H:%M:%S.000Z", time.gmtime(epoch))


def _line(version, activated_at, previous):
    return {"version": version, "activated_at": activated_at, "previous": previous}


class Base(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.tmp = Path(self._tmp.name)
        self.cache = self.tmp / "cache"
        self.cache.mkdir()
        self.settings = self.tmp / "settings.json"
        self.settings.write_text("{}", encoding="utf-8")
        self.registry = self.tmp / "installed_plugins.json"

    def setup_plugin(self, versions, current, last_updated, plugin=PLUGIN):
        """Create version dirs, crates/<plugin>/plugin.json at `current`, and a
        registry entry pointing at `current` with the given lastUpdated epoch."""
        pj = self.tmp / "crates" / plugin / ".claude-plugin" / "plugin.json"
        pj.parent.mkdir(parents=True, exist_ok=True)
        pj.write_text(json.dumps({"name": plugin, "version": current}), encoding="utf-8")
        for v in versions:
            d = self.cache / plugin / v
            d.mkdir(parents=True, exist_ok=True)
            (d / "payload.txt").write_text(v, encoding="utf-8")
        reg = {}
        if self.registry.exists():
            reg = json.loads(self.registry.read_text())
        reg.setdefault("plugins", {})[f"{plugin}@yukineko"] = [
            {
                "scope": "user",
                "installPath": str(self.cache / plugin / current),
                "version": current,
                "lastUpdated": _iso(last_updated),
            }
        ]
        self.registry.write_text(json.dumps(reg), encoding="utf-8")

    def write_ledger(self, lines, plugin=PLUGIN, raw=None):
        p = self.cache / plugin / LEDGER
        p.parent.mkdir(parents=True, exist_ok=True)
        if raw is not None:
            p.write_text(raw, encoding="utf-8")
        else:
            p.write_text("".join(json.dumps(x) + "\n" for x in lines), encoding="utf-8")

    def probe(self, *claude_starts, others=()):
        f = self.tmp / "probe-output.txt"
        rows = [SELF_LINE]
        for i, s in enumerate(claude_starts):
            rows.append(f"{9001 + i} {s} claude")
        for i, s in enumerate(others):
            rows.append(f"{9501 + i} {s} zsh")
        f.write_text("".join(r + "\n" for r in rows), encoding="utf-8")
        return f"cat '{f}'"

    def run_prune(self, probe, *extra):
        env = {
            "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
            "HOME": str(self.tmp / "home"),
            "CLAUDE_PLUGIN_CACHE": str(self.cache),
            "CLAUDE_PLUGIN_REGISTRY": str(self.registry),
            "CLAUDE_SETTINGS_JSON": str(self.settings),
            "PLUGIN_CACHE_PROC_LIST_PROBE": probe,
        }
        p = subprocess.run(
            [sys.executable, str(PRUNE), "--repo", str(self.tmp), "--cache",
             str(self.cache), "--registry", str(self.registry), *extra],
            env=env, capture_output=True, text=True, timeout=60,
        )
        return p.stdout, p.stderr, p.returncode

    def vdir(self, v, plugin=PLUGIN):
        return self.cache / plugin / v

    def assertExists(self, v, ctx):
        self.assertTrue(self.vdir(v).is_dir(), f"{v} must be KEPT. {ctx}")

    def assertGone(self, v, ctx):
        self.assertFalse(self.vdir(v).exists(), f"{v} must be PRUNED. {ctx}")


class H1H2H3H4Intervals(Base):
    CHAIN = [
        _line("1.0.0", 1000, None),
        _line("2.0.0", 2000, "1.0.0"),
        _line("3.0.0", 3000, "2.0.0"),
    ]

    def _chain3(self):
        # registry lastUpdated == latest repoint, as rollout writes it
        self.setup_plugin(["1.0.0", "2.0.0", "3.0.0"], "3.0.0", 3000)
        self.write_ledger(self.CHAIN)

    def test_H1_session_during_v1_holds_only_v1(self):
        self._chain3()
        out, err, rc = self.run_prune(self.probe(1500))  # act(V1)<=1500<sup(V1)=2000
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", ctx)
        self.assertGone("2.0.0", ctx)  # activated after the session started
        self.assertExists("3.0.0", ctx)  # current
        self.assertEqual(rc, 0, ctx)

    def test_H1b_session_during_v2_holds_only_v2(self):
        self._chain3()
        out, err, rc = self.run_prune(self.probe(2500))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertGone("1.0.0", ctx)  # superseded before the session began
        self.assertExists("2.0.0", ctx)
        self.assertEqual(rc, 0, ctx)

    def test_H2_no_older_claude_prunes_every_superseded_dir(self):
        self._chain3()
        # no claude at all; a claude newer than the latest repoint; and a
        # non-claude process older than everything. None may hold anything.
        for label, probe in {
            "no-claude": self.probe(others=(10,)),
            "claude-after-all": self.probe(5000, others=(10,)),
        }.items():
            with self.subTest(label):
                for v in ("1.0.0", "2.0.0"):
                    self.vdir(v).mkdir(exist_ok=True)
                    (self.vdir(v) / "payload.txt").write_text(v)
                out, err, rc = self.run_prune(probe)
                ctx = f"{label}: rc={rc} out={out!r} err={err!r}"
                self.assertGone("1.0.0", ctx)
                self.assertGone("2.0.0", ctx)
                self.assertExists("3.0.0", ctx)
                self.assertEqual(rc, 0, ctx)

    def test_H3_continuous_rollout_holds_only_the_version_the_session_started_on(self):
        # THE over-holding regression: a session older than the newest rollout
        # (lastUpdated = act(V5)) must NOT pin V1..V3.
        vs = ["1.0.0", "2.0.0", "3.0.0", "4.0.0", "5.0.0"]
        self.setup_plugin(vs, "5.0.0", 5000)
        self.write_ledger(
            [_line(v, 1000 * (i + 1), vs[i - 1] if i else None) for i, v in enumerate(vs)]
        )
        out, err, rc = self.run_prune(self.probe(4500))  # during V4
        ctx = f"rc={rc} out={out!r} err={err!r}"
        for v in ("1.0.0", "2.0.0", "3.0.0"):
            self.assertGone(v, ctx)
        self.assertExists("4.0.0", ctx)
        self.assertExists("5.0.0", ctx)
        self.assertEqual(rc, 0, ctx)

    def test_H4_boundaries_start_equal_activated_holds_start_equal_superseded_does_not(self):
        self._chain3()
        # S == act(V1): holds V1.
        out, err, rc = self.run_prune(self.probe(1000), "--dry-run")
        self.assertNotIn("1.0.0", self._would_remove(out), f"out={out!r} err={err!r}")
        # S == sup(V1) == act(V2): V1 free, V2 held (S == act(V2)).
        out, err, rc = self.run_prune(self.probe(2000), "--dry-run")
        self.assertIn("1.0.0", self._would_remove(out), f"out={out!r} err={err!r}")
        self.assertNotIn("2.0.0", self._would_remove(out), f"out={out!r} err={err!r}")
        # and for real, to observe the filesystem rather than the preview
        out, err, rc = self.run_prune(self.probe(2000))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertGone("1.0.0", ctx)
        self.assertExists("2.0.0", ctx)

    @staticmethod
    def _would_remove(out):
        return "\n".join(l for l in out.splitlines() if l.startswith("[dry-run] would remove"))

    def test_extra_missing_activation_is_minus_infinity(self):
        # Only the repoint that replaced 2.0.0 is recorded: sup(2.0.0)=3000 is
        # known, act(2.0.0) is not -> hold iff S < 3000.
        self.setup_plugin(["2.0.0", "3.0.0"], "3.0.0", 3000)
        self.write_ledger([_line("3.0.0", 3000, "2.0.0")])
        out, err, rc = self.run_prune(self.probe(10))
        self.assertExists("2.0.0", f"rc={rc} out={out!r} err={err!r}")
        out, err, rc = self.run_prune(self.probe(3000))
        self.assertGone("2.0.0", f"rc={rc} out={out!r} err={err!r}")
        self.assertEqual(rc, 0, f"out={out!r} err={err!r}")

    def test_extra_other_plugin_ledger_does_not_leak(self):
        # A session pinned to plugin A's old dir says nothing about plugin B.
        self.setup_plugin(["1.0.0", "2.0.0"], "2.0.0", 2000, plugin="alpha")
        self.write_ledger([_line("1.0.0", 1000, None), _line("2.0.0", 2000, "1.0.0")],
                          plugin="alpha")
        self.setup_plugin(["1.0.0", "2.0.0"], "2.0.0", 2000, plugin="beta")
        self.write_ledger([_line("1.0.0", 100, None), _line("2.0.0", 200, "1.0.0")],
                          plugin="beta")
        out, err, rc = self.run_prune(self.probe(1500))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertTrue((self.cache / "alpha" / "1.0.0").is_dir(), "alpha/1.0.0 held " + ctx)
        self.assertFalse((self.cache / "beta" / "1.0.0").exists(), "beta/1.0.0 free " + ctx)


class H6LegacyNoHistory(Base):
    def _legacy(self, ledger_lines=None):
        # 0.3.21 predates the ledger; registry lastUpdated = 5000
        self.setup_plugin(["0.3.21", "0.3.22"], "0.3.22", 5000)
        if ledger_lines is not None:
            self.write_ledger(ledger_lines)

    def test_H6_no_claude_older_than_lastupdated_prunes(self):
        for label, lines in {"no-ledger": None,
                             "ledger-not-naming-it": [_line("0.3.22", 5000, "0.3.20")]}.items():
            with self.subTest(label):
                self._legacy(lines)
                self.vdir("0.3.21").mkdir(exist_ok=True)
                out, err, rc = self.run_prune(self.probe(6000))
                ctx = f"{label}: rc={rc} out={out!r} err={err!r}"
                self.assertGone("0.3.21", ctx)
                self.assertEqual(rc, 0, ctx)

    def test_H6_claude_older_than_lastupdated_keeps_and_names_it(self):
        for label, lines in {"no-ledger": None,
                             "ledger-not-naming-it": [_line("0.3.22", 5000, "0.3.20")]}.items():
            with self.subTest(label):
                self._legacy(lines)
                self.vdir("0.3.21").mkdir(exist_ok=True)
                out, err, rc = self.run_prune(self.probe(4000))
                ctx = f"{label}: rc={rc} out={out!r} err={err!r}"
                self.assertExists("0.3.21", ctx)
                self.assertIn("KEPT backlog/0.3.21:", err, ctx)
                self.assertIn("no version history", err, ctx)


class H7CorruptLedger(Base):
    def _chain(self):
        self.setup_plugin(["1.0.0", "2.0.0", "3.0.0"], "3.0.0", 3000)

    def test_H7_corrupt_ledger_keeps_superseded_dirs_and_fails(self):
        good = json.dumps(_line("2.0.0", 2000, "1.0.0")) + "\n"
        cases = {
            "garbage-line": good + "this is not json\n",
            "json-not-object": good + "[1, 2, 3]\n",
            "garbage-first": "{{{\n" + good,
            "truncated-json": good + '{"version": "3.0.0", "activated_at": 30',
        }
        for label, raw in cases.items():
            with self.subTest(label):
                self._chain()
                for v in ("1.0.0", "2.0.0"):
                    self.vdir(v).mkdir(exist_ok=True)
                self.write_ledger(None, raw=raw)
                # NO claude alive besides prune itself: still must keep.
                out, err, rc = self.run_prune(self.probe(others=(10,)))
                ctx = f"{label}: rc={rc} out={out!r} err={err!r}"
                self.assertExists("1.0.0", ctx)
                self.assertExists("2.0.0", ctx)
                self.assertExists("3.0.0", ctx)
                self.assertNotEqual(rc, 0, ctx)

    def test_H7_unreadable_ledger_keeps_and_fails(self):
        self._chain()
        # a directory where the file should be: open() fails with an OSError
        (self.cache / PLUGIN / LEDGER).mkdir()
        out, err, rc = self.run_prune(self.probe(others=(10,)))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", ctx)
        self.assertExists("2.0.0", ctx)
        self.assertNotEqual(rc, 0, ctx)

    def test_H7_corruption_is_per_plugin(self):
        self.setup_plugin(["1.0.0", "2.0.0"], "2.0.0", 2000, plugin="alpha")
        self.write_ledger(None, raw="garbage\n", plugin="alpha")
        self.setup_plugin(["1.0.0", "2.0.0"], "2.0.0", 2000, plugin="beta")
        self.write_ledger([_line("1.0.0", 100, None), _line("2.0.0", 200, "1.0.0")],
                          plugin="beta")
        out, err, rc = self.run_prune(self.probe(others=(10,)))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertTrue((self.cache / "alpha" / "1.0.0").is_dir(), "corrupt plugin kept " + ctx)
        self.assertFalse((self.cache / "beta" / "1.0.0").exists(), "healthy plugin pruned " + ctx)
        self.assertNotEqual(rc, 0, ctx)


class H5RolloutWritesLedger(unittest.TestCase):
    """Drives the REAL repoint path: scripts/rollout-plugins.sh -> its
    registry_patch python heredoc, against temp HOME/cache/registry (same
    hermetic recipe as test_rollout_registry_safety.ConcurrentRolloutIsExcluded;
    taskprog is a non-gate crate so no canary/overwatch is needed)."""

    NAME = "taskprog"

    def _setup(self, tmp, with_prior=True):
        home, cache = tmp / "home", tmp / "cache" / "yukineko"
        reg = tmp / "installed_plugins.json"
        home.mkdir()
        cache.mkdir(parents=True)
        entries = {}
        if with_prior:
            d = cache / self.NAME / "0.0.1-prior"
            d.mkdir(parents=True)
            (d / "marker").write_text("prior")
            entries[f"{self.NAME}@yukineko"] = [{
                "scope": "user", "installPath": str(d), "version": "0.0.1-prior",
                "lastUpdated": "2026-01-01T00:00:00.000Z"}]
        reg.write_text(json.dumps({"version": 1, "plugins": entries}))
        return home, cache, reg

    def _rollout(self, home, cache, reg):
        env = dict(os.environ)
        env.update(HOME=str(home), CLAUDE_PLUGIN_CACHE=str(cache),
                   CLAUDE_PLUGIN_REGISTRY=str(reg),
                   CLAUDE_SETTINGS_JSON=str(home / "settings.json"),
                   PLUGIN_CACHE_PROC_LIST_PROBE="echo '1 1 python3'")
        return subprocess.run(
            ["bash", str(ROLLOUT), "--plugin", self.NAME, "--no-rebuild", "--no-sync"],
            env=env, capture_output=True, text=True, cwd=str(REPO), timeout=300)

    def _crate_version(self):
        pj = REPO / "crates" / self.NAME / ".claude-plugin" / "plugin.json"
        return json.loads(pj.read_text())["version"]

    @staticmethod
    def _read_ledger(path):
        return [json.loads(l) for l in path.read_text().splitlines() if l.strip()]

    def test_H5_repoint_appends_ledger_line_with_previous(self):
        with tempfile.TemporaryDirectory() as t:
            home, cache, reg = self._setup(Path(t))
            new = self._crate_version()
            before = int(time.time())
            r = self._rollout(home, cache, reg)
            after = int(time.time())
            log = r.stdout + r.stderr
            self.assertIn(f"copied {self.NAME} ->", log, f"rollout did not repoint\n{log}")
            led = cache / self.NAME / LEDGER
            self.assertTrue(led.is_file(), f"no ledger written at {led}\n{log}")
            rows = self._read_ledger(led)
            self.assertEqual(len(rows), 1, f"exactly one repoint -> one line: {rows}")
            row = rows[0]
            self.assertEqual(row["version"], new)
            self.assertEqual(row["previous"], "0.0.1-prior")
            self.assertIsInstance(row["activated_at"], int)
            self.assertNotIsInstance(row["activated_at"], bool)
            self.assertTrue(before - 1 <= row["activated_at"] <= after + 1,
                            f"activated_at {row['activated_at']} outside [{before},{after}]")

    def test_H5_second_run_without_version_change_appends_nothing(self):
        with tempfile.TemporaryDirectory() as t:
            home, cache, reg = self._setup(Path(t))
            self._rollout(home, cache, reg)
            led = cache / self.NAME / LEDGER
            self.assertTrue(led.is_file(), "first run wrote no ledger")
            first = led.read_text()
            r = self._rollout(home, cache, reg)
            self.assertEqual(led.read_text(), first,
                             f"idempotent no-op rerun must not append\n{r.stdout}{r.stderr}")

    def test_H5_dry_run_writes_no_ledger(self):
        with tempfile.TemporaryDirectory() as t:
            home, cache, reg = self._setup(Path(t))
            env = dict(os.environ)
            env.update(HOME=str(home), CLAUDE_PLUGIN_CACHE=str(cache),
                       CLAUDE_PLUGIN_REGISTRY=str(reg),
                       CLAUDE_SETTINGS_JSON=str(home / "settings.json"))
            r = subprocess.run(
                ["bash", str(ROLLOUT), "--plugin", self.NAME, "--dry-run",
                 "--no-rebuild", "--no-sync"],
                env=env, capture_output=True, text=True, cwd=str(REPO), timeout=300)
            self.assertFalse((cache / self.NAME / LEDGER).exists(),
                             f"dry-run must not write the ledger\n{r.stdout}{r.stderr}")

    def test_H5_first_install_records_previous_null(self):
        with tempfile.TemporaryDirectory() as t:
            home, cache, reg = self._setup(Path(t), with_prior=False)
            new = self._crate_version()
            r = self._rollout(home, cache, reg)
            led = cache / self.NAME / LEDGER
            self.assertTrue(led.is_file(), f"no ledger on first install\n{r.stdout}{r.stderr}")
            row = self._read_ledger(led)[-1]
            self.assertEqual(row["version"], new)
            self.assertIsNone(row["previous"])


if __name__ == "__main__":
    unittest.main()
