#!/usr/bin/env python3
"""Independent tests for the v2 per-version hold behaviour that
test_prune_version_history.py does not pin (backlog 18fe626f, commit 8872707e).

Written by an author who did not write the implementation, from the stated
contract, not from reading the code's intent:

(a) LEDGER GAP. A ledger line whose `previous` is not the version the line
    before it activated proves a repoint the ledger did not record: during the
    time between the two lines ANY version may have been current, so a claude
    that started in that gap holds every superseded dir that could have been
    current. A first line with a non-null `previous` makes (-inf, first) a gap.
    Lost lines may only WIDEN holds, never release one.
(b) REGISTRY MOVED ON WITHOUT THE LEDGER. When the registry no longer points at
    the version the ledger last activated, the open end of the history is
    bounded by the registry `lastUpdated`; a missing `lastUpdated`, or one
    earlier than the last ledger line, leaves the dir UNDETERMINED: kept, named
    `KEPT <plugin>/<ver>:` on stderr, exit 1 ("not clean").
(c) PS START IS A RANGE. A claude whose start time is within 1s of an
    activated_at / superseded_at must resolve to the HOLD side. Driven through
    the real `ps` code path (a fake `ps` on PATH, no probe seam).
(d) LEDGER APPEND FAILURE in the real scripts/rollout-plugins.sh: registry
    restored from backup, this run's ledger lines undone, exit 1.
(e) LEGACY dir bound = the LATER of registry lastUpdated and the ledger's last
    line time.
(g) check-plugin-rollout.py check_stale_version_dirs applies the same rule.
"""
import importlib.util
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path
from unittest import mock

HERE = Path(__file__).resolve().parent
sys.path.insert(0, str(HERE))
import test_prune_version_history as tv  # noqa: E402

REPO = HERE.parent
ROLLOUT = HERE / "rollout-plugins.sh"
_line, _iso, LEDGER, PLUGIN = tv._line, tv._iso, tv.LEDGER, tv.PLUGIN


class Base(tv.Base):
    def set_registry_entry(self, plugin, **fields):
        """Edit the plugin's registry entry (None deletes the key)."""
        reg = json.loads(self.registry.read_text())
        ent = reg["plugins"][f"{plugin}@yukineko"][0]
        for k, v in fields.items():
            if v is None:
                ent.pop(k, None)
            else:
                ent[k] = v
        self.registry.write_text(json.dumps(reg), encoding="utf-8")

    def dirs_present(self, versions, plugin=PLUGIN):
        return {v for v in versions if (self.cache / plugin / v).is_dir()}


V = ["1.0.0", "2.0.0", "3.0.0"]


class AGapsOnlyWiden(Base):
    def test_a_middle_gap_session_in_gap_holds_every_superseded_dir(self):
        # 3.0.0's line says previous=9.9.9, but the line before activated
        # 2.0.0: (2000, 4000) is a gap in which ANY version may have been current.
        self.setup_plugin(V, "3.0.0", 4000)
        self.write_ledger([_line("1.0.0", 1000, None), _line("2.0.0", 2000, "1.0.0"),
                           _line("3.0.0", 4000, "9.9.9")])
        out, err, rc = self.run_prune(self.probe(3000))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", "held through the gap " + ctx)
        self.assertExists("2.0.0", ctx)
        self.assertEqual(rc, 0, ctx)

    def test_a_gap_does_not_hold_sessions_outside_it(self):
        self.setup_plugin(V, "3.0.0", 4000)
        self.write_ledger([_line("1.0.0", 1000, None), _line("2.0.0", 2000, "1.0.0"),
                           _line("3.0.0", 4000, "9.9.9")])
        # inside [1000,2000): V1 only; the gap starts at 2000.
        out, err, rc = self.run_prune(self.probe(1500))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", ctx)
        self.assertGone("2.0.0", "no over-holding before the gap " + ctx)
        # after the last line: nothing held.
        for v in ("2.0.0",):
            (self.cache / PLUGIN / v).mkdir(exist_ok=True)
        out, err, rc = self.run_prune(self.probe(4500))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertGone("1.0.0", ctx)
        self.assertGone("2.0.0", ctx)

    def test_a_first_line_with_non_null_previous_is_a_gap_from_minus_infinity(self):
        # Ledger starts at 2.0.0 (previous=1.0.0): before 2000 something we
        # never recorded was current. A very old session holds BOTH.
        self.setup_plugin(V, "3.0.0", 3000)
        self.write_ledger([_line("2.0.0", 2000, "1.0.0"), _line("3.0.0", 3000, "2.0.0")])
        out, err, rc = self.run_prune(self.probe(10))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", ctx)
        self.assertExists("2.0.0", "gap from -inf also covers named 2.0.0 " + ctx)
        self.assertEqual(rc, 0, ctx)
        # a session after the gap and inside 2.0.0's own interval: V1 free.
        out, err, rc = self.run_prune(self.probe(2500))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertGone("1.0.0", ctx)
        self.assertExists("2.0.0", ctx)

    def test_a_lost_middle_line_only_widens(self):
        # 2.0.0's own line was lost: 3.0.0 says previous=2.0.0 but the line
        # before activated 1.0.0.
        self.setup_plugin(V, "3.0.0", 3000)
        self.write_ledger([_line("1.0.0", 1000, None), _line("3.0.0", 3000, "2.0.0")])
        out, err, rc = self.run_prune(self.probe(2000))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", ctx)
        self.assertExists("2.0.0", ctx)
        out, err, rc = self.run_prune(self.probe(3500))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertGone("1.0.0", ctx)
        self.assertGone("2.0.0", ctx)


class BRegistryMovedOnWithoutLedger(Base):
    """Ledger ends at 2.0.0; the registry points at 3.0.0 (repointed by
    something that did not write the ledger)."""

    LEDGER_LINES = [_line("1.0.0", 1000, None), _line("2.0.0", 2000, "1.0.0")]

    def _setup(self, last_updated=3000):
        self.setup_plugin(V, "3.0.0", last_updated)
        self.write_ledger(self.LEDGER_LINES)

    def test_b_open_end_is_bounded_by_registry_last_updated(self):
        self._setup(3000)
        # during the unrecorded tail (2000..3000) ANY version may be current
        out, err, rc = self.run_prune(self.probe(2500))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", ctx)
        self.assertExists("2.0.0", ctx)
        self.assertEqual(rc, 0, ctx)
        # at/after lastUpdated: bounded, so both released
        out, err, rc = self.run_prune(self.probe(3000))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertGone("1.0.0", ctx)
        self.assertGone("2.0.0", ctx)
        self.assertEqual(rc, 0, ctx)

    def test_b_tail_widening_does_not_hold_sessions_before_the_tail(self):
        self._setup(3000)
        out, err, rc = self.run_prune(self.probe(1500))  # only V1 was current
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", ctx)
        self.assertGone("2.0.0", ctx)

    def test_b_last_updated_missing_is_undetermined_kept_and_named(self):
        self._setup(3000)
        self.set_registry_entry(PLUGIN, lastUpdated=None)
        # no claude at all: still cannot tell, still kept
        out, err, rc = self.run_prune(self.probe(others=(10,)))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        for v in ("1.0.0", "2.0.0"):
            self.assertExists(v, ctx)
            self.assertIn(f"KEPT backlog/{v}:", err, ctx)
        self.assertEqual(rc, 1, ctx)  # undetermined is not clean

    def test_b_last_updated_earlier_than_last_ledger_line_is_undetermined(self):
        self._setup(1500)  # < 2000, the last ledger line
        out, err, rc = self.run_prune(self.probe(others=(10,)))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        for v in ("1.0.0", "2.0.0"):
            self.assertExists(v, ctx)
            self.assertIn(f"KEPT backlog/{v}:", err, ctx)
        self.assertEqual(rc, 1, ctx)

    def test_b_dry_run_also_reports_undetermined(self):
        self._setup(1500)
        out, err, rc = self.run_prune(self.probe(others=(10,)), "--dry-run")
        self.assertEqual(rc, 1, f"out={out!r} err={err!r}")
        self.assertNotIn("would remove", out)


class CPsStartIsARange(Base):
    """Drives the real `ps` path: a fake `ps` first on PATH prints the
    elapsed time of a claude that started at a fixed epoch S. The true start
    lies within ~1s of S either way, so any boundary within 1s must HOLD."""

    def setUp(self):
        super().setUp()
        self.S = int(time.time()) - 100
        fake = self.tmp / "fakebin"
        fake.mkdir()
        ps = fake / "ps"
        ps.write_text(
            "#!/bin/sh\n"
            f"n=$(( $(date +%s) - {self.S} ))\n"
            'printf "%d %02d:%02d claude\\n" 4242 $((n/60)) $((n%60))\n'
            'printf "%d 00:01 ps\\n" 4243\n',
            encoding="utf-8",
        )
        ps.chmod(0o755)
        self.fakebin = fake

    def run_ps(self):
        env = {
            "PATH": f"{self.fakebin}:{os.environ.get('PATH', '/usr/bin:/bin')}",
            "HOME": str(self.tmp / "home"),
            "CLAUDE_PLUGIN_CACHE": str(self.cache),
            "CLAUDE_PLUGIN_REGISTRY": str(self.registry),
            "CLAUDE_SETTINGS_JSON": str(self.settings),
        }
        p = subprocess.run(
            [sys.executable, str(tv.PRUNE), "--repo", str(self.tmp), "--cache",
             str(self.cache), "--registry", str(self.registry)],
            env=env, capture_output=True, text=True, timeout=60)
        return p.stdout, p.stderr, p.returncode

    def _chain(self, sup_v1, act_v2):
        # 1.0.0 superseded at sup_v1, 2.0.0 activated at act_v2 (a gap-free
        # chain needs them equal; unequal means a lost repoint -> use 3 lines)
        S = self.S
        self.setup_plugin(V, "3.0.0", S + 50)
        self.write_ledger([_line("1.0.0", S - 1000, None),
                           _line("2.0.0", sup_v1, "1.0.0"),
                           _line("3.0.0", S + 50, "2.0.0")])

    def test_c_start_equal_to_superseded_at_holds_old_version(self):
        # exactly: S == sup(1.0.0) releases 1.0.0; the +-1s range must hold it
        self._chain(self.S, self.S)
        out, err, rc = self.run_ps()
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", "rounding must never release " + ctx)
        self.assertExists("2.0.0", ctx)

    def test_c_start_one_second_before_activated_at_holds_new_version(self):
        # act(2.0.0) = S+1: exactly, S < act so 2.0.0 is NOT held; range holds
        self._chain(self.S + 1, self.S + 1)
        out, err, rc = self.run_ps()
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("2.0.0", "rounding must never release " + ctx)

    def test_c_tolerance_is_bounded_five_seconds_away_releases(self):
        # sup(1.0.0) = S-5 and act(2.0.0) = S+5 are NOT within rounding:
        # the session holds neither a version superseded 5s before it started
        # (that one is released) ...
        S = self.S
        self.setup_plugin(V, "3.0.0", S + 50)
        self.write_ledger([_line("1.0.0", S - 1000, None),
                           _line("2.0.0", S - 5, "1.0.0"),
                           _line("3.0.0", S + 50, "2.0.0")])
        out, err, rc = self.run_ps()
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertGone("1.0.0", "no over-holding beyond rounding " + ctx)
        self.assertExists("2.0.0", ctx)  # it started during 2.0.0

    def test_c_tolerance_is_bounded_activation_five_seconds_later_releases(self):
        S = self.S
        self.setup_plugin(V, "3.0.0", S + 50)
        self.write_ledger([_line("1.0.0", S - 1000, None),
                           _line("2.0.0", S + 5, "1.0.0"),
                           _line("3.0.0", S + 50, "2.0.0")])
        out, err, rc = self.run_ps()
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("1.0.0", ctx)
        self.assertGone("2.0.0", "activated 5s after the session began " + ctx)


class DAppendFailureRollsBack(unittest.TestCase):
    """Real scripts/rollout-plugins.sh, hermetic like test_prune_version_history
    H5. Two plugins are repointed in one registry_patch; one has a DIRECTORY at
    its ledger path so the append fails (EISDIR)."""

    NAMES = ("taskprog", "difflog")

    def _run(self, tmp, fail_name):
        home, cache, reg = tmp / "home", tmp / "cache" / "yukineko", tmp / "installed_plugins.json"
        home.mkdir()
        cache.mkdir(parents=True)
        entries = {}
        for n in self.NAMES:
            d = cache / n / "0.0.1-prior"
            d.mkdir(parents=True)
            (d / "marker").write_text("prior")
            entries[f"{n}@yukineko"] = [{
                "scope": "user", "installPath": str(d), "version": "0.0.1-prior",
                "lastUpdated": "2026-01-01T00:00:00.000Z"}]
        reg.write_text(json.dumps({"version": 1, "plugins": entries}))
        before_reg = reg.read_bytes()
        # the OTHER plugin has a pre-existing ledger that must come back byte-exact
        pre = {}
        for n in self.NAMES:
            led = cache / n / LEDGER
            if n == fail_name:
                led.mkdir()
            else:
                pre[n] = json.dumps({"version": "0.0.1-prior", "activated_at": 5,
                                     "previous": None}) + "\n"
                led.write_text(pre[n])
        env = dict(os.environ)
        env.update(HOME=str(home), CLAUDE_PLUGIN_CACHE=str(cache),
                   CLAUDE_PLUGIN_REGISTRY=str(reg),
                   CLAUDE_SETTINGS_JSON=str(home / "settings.json"),
                   PLUGIN_CACHE_PROC_LIST_PROBE="echo '1 1 python3'")
        args = ["bash", str(ROLLOUT), "--no-rebuild", "--no-sync"]
        for n in self.NAMES:
            args += ["--plugin", n]
        r = subprocess.run(args, env=env, capture_output=True, text=True,
                           cwd=str(REPO), timeout=300)
        return r, reg, before_reg, cache, pre

    def _check(self, fail_name):
        with tempfile.TemporaryDirectory() as t:
            r, reg, before_reg, cache, pre = self._run(Path(t), fail_name)
            log = f"rc={r.returncode}\n{r.stdout}\n{r.stderr}"
            self.assertEqual(r.returncode, 1, log)
            self.assertIn("version-history write failed", r.stderr, log)
            self.assertEqual(reg.read_bytes(), before_reg,
                             "registry must be restored from backup\n" + log)
            for n, text in pre.items():
                self.assertEqual((cache / n / LEDGER).read_text(), text,
                                 f"{n}: this run's ledger line must be truncated away\n{log}")
            self.assertTrue((cache / fail_name / LEDGER).is_dir(), log)

    # The rollout processes difflog before taskprog (observed), so failing on
    # taskprog is "append OK for difflog, then fail" (needs the undo), failing
    # on difflog is "fail before anything was appended".
    def test_d_failure_after_an_earlier_append_truncates_it(self):
        self._check("taskprog")

    def test_d_failure_before_any_append_leaves_state_untouched(self):
        self._check("difflog")

    def test_d_ledger_created_by_this_run_is_removed_on_failure(self):
        # difflog has NO ledger before and is appended first; taskprog's ledger
        # path is a directory so its append fails: the ledger difflog just
        # CREATED must not survive.
        with tempfile.TemporaryDirectory() as t:
            tmp = Path(t)
            home, cache, reg = tmp / "home", tmp / "cache" / "yukineko", tmp / "r.json"
            home.mkdir()
            cache.mkdir(parents=True)
            entries = {}
            for n in self.NAMES:
                d = cache / n / "0.0.1-prior"
                d.mkdir(parents=True)
                entries[f"{n}@yukineko"] = [{
                    "scope": "user", "installPath": str(d), "version": "0.0.1-prior",
                    "lastUpdated": "2026-01-01T00:00:00.000Z"}]
            reg.write_text(json.dumps({"version": 1, "plugins": entries}))
            (cache / "taskprog" / LEDGER).mkdir()
            env = dict(os.environ)
            env.update(HOME=str(home), CLAUDE_PLUGIN_CACHE=str(cache),
                       CLAUDE_PLUGIN_REGISTRY=str(reg),
                       CLAUDE_SETTINGS_JSON=str(home / "settings.json"),
                       PLUGIN_CACHE_PROC_LIST_PROBE="echo '1 1 python3'")
            r = subprocess.run(
                ["bash", str(ROLLOUT), "--no-rebuild", "--no-sync",
                 "--plugin", "taskprog", "--plugin", "difflog"],
                env=env, capture_output=True, text=True, cwd=str(REPO), timeout=300)
            log = f"rc={r.returncode}\n{r.stdout}\n{r.stderr}"
            self.assertEqual(r.returncode, 1, log)
            self.assertFalse((cache / "difflog" / LEDGER).exists(), log)


class ELegacyBoundIsLaterOfLastUpdatedAndLedgerTail(Base):
    def _setup(self, last_updated, ledger_at):
        # 0.3.21 predates the ledger (no line names it); 0.3.23 is current.
        self.setup_plugin(["0.3.21", "0.3.23"], "0.3.23", last_updated)
        self.write_ledger([_line("0.3.23", ledger_at, "0.3.22")])

    def test_e_ledger_tail_later_than_last_updated_is_the_bound(self):
        self._setup(last_updated=5000, ledger_at=6000)
        out, err, rc = self.run_prune(self.probe(5500))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("0.3.21", ctx)
        self.assertIn("KEPT backlog/0.3.21:", err, ctx)
        out, err, rc = self.run_prune(self.probe(6000))
        self.assertGone("0.3.21", f"rc={rc} out={out!r} err={err!r}")

    def test_e_last_updated_later_than_ledger_tail_is_the_bound(self):
        self._setup(last_updated=7000, ledger_at=6000)
        out, err, rc = self.run_prune(self.probe(6500))
        ctx = f"rc={rc} out={out!r} err={err!r}"
        self.assertExists("0.3.21", ctx)
        out, err, rc = self.run_prune(self.probe(7000))
        self.assertGone("0.3.21", f"rc={rc} out={out!r} err={err!r}")


def _load_gate():
    spec = importlib.util.spec_from_file_location(
        "check_plugin_rollout_extras", HERE / "check-plugin-rollout.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m


class GGateAppliesSameRule(Base):
    @classmethod
    def setUpClass(cls):
        cls.cpr = _load_gate()

    def gate(self, probe):
        env = {"PLUGIN_CACHE_PROC_LIST_PROBE": probe,
               "CLAUDE_PLUGIN_REGISTRY": str(self.registry),
               "CLAUDE_SETTINGS_JSON": str(self.settings)}
        with mock.patch.dict(os.environ, env), \
                mock.patch.object(self.cpr, "CRATES", str(self.tmp / "crates")), \
                mock.patch.object(self.cpr, "PLUGIN_CACHE_ROOT", str(self.cache)):
            return self.cpr.check_stale_version_dirs()

    @staticmethod
    def removable_problems(problems):
        return [p for p in problems if "still in the cache and removable" in p]

    def _two(self):
        self.setup_plugin(["1.0.0", "2.0.0"], "2.0.0", 2000)
        self.write_ledger([_line("1.0.0", 1000, None), _line("2.0.0", 2000, "1.0.0")])

    def test_g_dir_prune_keeps_is_not_removable_for_the_gate(self):
        self._two()
        probe = self.probe(1500)
        out, err, rc = self.run_prune(probe, "--dry-run")
        self.assertNotIn("would remove", out, f"prune must keep it: {out!r}")
        problems, _n = self.gate(probe)
        self.assertEqual(self.removable_problems(problems), [], problems)

    def test_g_dir_prune_would_prune_is_removable_for_the_gate(self):
        self._two()
        probe = self.probe(2500)
        out, err, rc = self.run_prune(probe, "--dry-run")
        self.assertIn("would remove", out, out)
        problems, _n = self.gate(probe)
        rp = self.removable_problems(problems)
        self.assertEqual(len(rp), 1, problems)
        self.assertIn("1 superseded plugin version dir(s)", rp[0])

    def test_g_gate_uses_per_version_interval_not_blanket(self):
        # continuous rollout: session during 4.0.0 -> 1,2,3 removable (3 dirs)
        vs = ["1.0.0", "2.0.0", "3.0.0", "4.0.0", "5.0.0"]
        self.setup_plugin(vs, "5.0.0", 5000)
        self.write_ledger([_line(v, 1000 * (i + 1), vs[i - 1] if i else None)
                           for i, v in enumerate(vs)])
        problems, _n = self.gate(self.probe(4500))
        rp = self.removable_problems(problems)
        self.assertEqual(len(rp), 1, problems)
        self.assertIn("3 superseded plugin version dir(s)", rp[0])

    def test_g_gate_gap_holds_like_prune(self):
        self.setup_plugin(V, "3.0.0", 4000)
        self.write_ledger([_line("1.0.0", 1000, None), _line("2.0.0", 2000, "1.0.0"),
                           _line("3.0.0", 4000, "9.9.9")])
        problems, _n = self.gate(self.probe(3000))
        self.assertEqual(self.removable_problems(problems), [], problems)

    def test_g_gate_legacy_bound_is_later_of_last_updated_and_ledger_tail(self):
        self.setup_plugin(["0.3.21", "0.3.23"], "0.3.23", 5000)
        self.write_ledger([_line("0.3.23", 6000, "0.3.22")])
        problems, _n = self.gate(self.probe(5500))
        self.assertEqual(self.removable_problems(problems), [], problems)
        problems, _n = self.gate(self.probe(6000))
        self.assertEqual(len(self.removable_problems(problems)), 1, problems)

    def test_g_gate_reports_undetermined_registry_tail(self):
        self.setup_plugin(V, "3.0.0", 1500)
        self.write_ledger([_line("1.0.0", 1000, None), _line("2.0.0", 2000, "1.0.0")])
        problems, _n = self.gate(self.probe(others=(10,)))
        self.assertTrue(any("cannot determine whether a live session" in p for p in problems),
                        problems)


if __name__ == "__main__":
    unittest.main()
