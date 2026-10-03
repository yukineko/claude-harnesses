#!/usr/bin/env python3
"""Registry-safety tests for the rollout scripts.

Backlog e3366b5c: prune-plugin-cache.py decides "current" from the invoking
tree's plugin.json only and never reads installed_plugins.json, so a version
dir that ANOTHER session just registered is deleted as "stale" and the plugin
goes dark (registry points at a missing dir).

Backlog ba5794b3: a canary halt (exit 4 / exit 5) leaves already-passed stages
repointed at fresh version dirs that have no host binary (seeding happens only
after the stage loop), so those plugins are registered but exec nothing.

HERMETIC: every path is a temp dir; HOME, CLAUDE_PLUGIN_CACHE,
CLAUDE_PLUGIN_REGISTRY and CLAUDE_SETTINGS_JSON are pointed into it. The real
~/.claude is never read or written.
"""
import json
import os
import platform
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
PRUNE = HERE / "prune-plugin-cache.py"
ROLLOUT = HERE / "rollout-plugins.sh"


def _env(home, cache, registry, **extra):
    env = dict(os.environ)
    env.update(
        HOME=str(home),
        CLAUDE_PLUGIN_CACHE=str(cache),
        CLAUDE_PLUGIN_REGISTRY=str(registry),
        CLAUDE_SETTINGS_JSON=str(Path(home) / "settings.json"),  # absent: no pins
        # 18fe626f: the pruner now asks the process table which `claude`
        # sessions are older than a dir's supersession. Pin it to a fixed
        # answer with no claude in it, so these registry tests neither read
        # nor depend on the real sessions running on this machine.
        PLUGIN_CACHE_PROC_LIST_PROBE="echo '1 1 python3'",
    )
    env.update(extra)
    return env


def _write_registry(path, entries):
    """entries: {name: (version, installPath)}"""
    plugins = {
        f"{n}@yukineko": [
            {
                "scope": "user",
                "installPath": str(p),
                "version": v,
                # what rollout-plugins.sh writes on every repoint; the
                # session-age hold (18fe626f) reads it as superseded-at
                "lastUpdated": "2026-01-01T00:00:00.000Z",
            }
        ]
        for n, (v, p) in entries.items()
    }
    Path(path).write_text(json.dumps({"version": 1, "plugins": plugins}))


class PruneRespectsRegistry(unittest.TestCase):
    """e3366b5c"""

    def _sandbox(self, tmp):
        tmp = Path(tmp)
        home, cache = tmp / "home", tmp / "cache" / "yukineko"
        repo = tmp / "repo"
        for d in (home, cache):
            d.mkdir(parents=True)
        pj = repo / "crates" / "alpha" / ".claude-plugin" / "plugin.json"
        pj.parent.mkdir(parents=True)
        # THIS tree (session B) is at 0.2.55 ...
        pj.write_text(json.dumps({"name": "alpha", "version": "0.2.55"}))
        for v in ("0.2.54", "0.2.55", "0.2.56"):
            d = cache / "alpha" / v
            d.mkdir(parents=True)
            (d / "payload.txt").write_text(v)
        return tmp, home, cache, repo

    def _prune(self, home, cache, registry, repo):
        return subprocess.run(
            [sys.executable, str(PRUNE), "--repo", str(repo), "--cache", str(cache)],
            env=_env(home, cache, registry),
            capture_output=True,
            text=True,
        )

    def test_registry_pointed_version_newer_than_tree_survives_prune(self):
        with tempfile.TemporaryDirectory() as t:
            tmp, home, cache, repo = self._sandbox(t)
            reg = tmp / "installed_plugins.json"
            # ... but session A already registered 0.2.56.
            _write_registry(reg, {"alpha": ("0.2.56", cache / "alpha" / "0.2.56")})
            r = self._prune(home, cache, reg, repo)
            self.assertTrue(
                (cache / "alpha" / "0.2.56").is_dir(),
                "prune deleted the version dir installed_plugins.json points at "
                f"(plugin goes dark).\nstdout={r.stdout}\nstderr={r.stderr}",
            )

    def test_control_unreferenced_stale_dir_is_still_pruned(self):
        """Control: a rule 'never delete anything' would pass the test above.
        The genuinely stale, unreferenced 0.2.54 must still go."""
        with tempfile.TemporaryDirectory() as t:
            tmp, home, cache, repo = self._sandbox(t)
            reg = tmp / "installed_plugins.json"
            _write_registry(reg, {"alpha": ("0.2.56", cache / "alpha" / "0.2.56")})
            r = self._prune(home, cache, reg, repo)
            self.assertFalse(
                (cache / "alpha" / "0.2.54").exists(),
                f"control failed: stale unreferenced dir was kept.\n{r.stdout}\n{r.stderr}",
            )
            self.assertTrue((cache / "alpha" / "0.2.55").is_dir(), "current dir removed")

    def test_unreadable_registry_prunes_nothing(self):
        """Cannot determine what the registry references -> restrictive side:
        keep every dir (CLAUDE.md 3)."""
        with tempfile.TemporaryDirectory() as t:
            tmp, home, cache, repo = self._sandbox(t)
            reg = tmp / "installed_plugins.json"
            reg.write_text("{ this is not json")
            r = self._prune(home, cache, reg, repo)
            for v in ("0.2.54", "0.2.55", "0.2.56"):
                self.assertTrue(
                    (cache / "alpha" / v).is_dir(),
                    f"{v} pruned although the registry could not be read.\n"
                    f"{r.stdout}\n{r.stderr}",
                )


def _find_overwatch():
    cands = [os.environ.get("OVERWATCH_BIN", "")]
    cands += [str(REPO / "target" / k / "overwatch") for k in ("release", "debug")]
    try:
        common = subprocess.run(
            ["git", "-C", str(REPO), "rev-parse", "--git-common-dir"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        main = (Path(REPO) / common).resolve().parent
        cands += [str(main / "target" / k / "overwatch") for k in ("release", "debug")]
    except Exception:
        pass
    for c in cands:
        if c and os.access(c, os.X_OK) and Path(c).is_file():
            return c
    return None


class CanaryHaltLeavesNoDarkEntry(unittest.TestCase):
    """ba5794b3"""

    STAGE0, STAGE1 = "blastguard", "backlog"  # marketplace order: blastguard first

    def test_halt_at_stage1_leaves_every_registry_entry_with_host_binary(self):
        ow = _find_overwatch()
        # A missing overwatch is a failed measurement, not a pass or a skip.
        self.assertIsNotNone(ow, "no overwatch binary found; set OVERWATCH_BIN")
        suffix = (
            f"{platform.system().lower()}-"
            f"{'arm64' if platform.machine() in ('arm64', 'aarch64') else 'x86_64'}"
        )
        with tempfile.TemporaryDirectory() as t:
            tmp = Path(t)
            home, cache = tmp / "home", tmp / "cache" / "yukineko"
            reg = tmp / "installed_plugins.json"
            home.mkdir()
            prior = {}
            for n in (self.STAGE0, self.STAGE1):
                d = cache / n / "0.0.1-prior"
                (d / "bin").mkdir(parents=True)
                hb = d / "bin" / f"{n}-{suffix}"
                hb.write_text("#!/bin/sh\nexit 0\n")
                hb.chmod(hb.stat().st_mode | stat.S_IXUSR)
                prior[n] = ("0.0.1-prior", d)
            _write_registry(reg, prior)

            # Proxy overwatch: 1st canary-gate (stage 0) is real (PROCEED, no
            # violations in the sandbox HOME); 2nd (stage 1) returns 3 = ROLLBACK.
            counter = tmp / "gate-calls"
            wrapper = tmp / "overwatch-wrapper"
            wrapper.write_text(
                "#!/bin/sh\n"
                'if [ "$1" = canary-gate ]; then\n'
                f'  echo x >> "{counter}"\n'
                f'  n=$(wc -l < "{counter}")\n'
                '  if [ "$n" -ge 2 ]; then echo "verdict: ROLLBACK (test)"; exit 3; fi\n'
                "fi\n"
                f'exec "{ow}" "$@"\n'
            )
            wrapper.chmod(0o755)

            env = _env(home, cache, reg, OVERWATCH_BIN=str(wrapper))
            p = subprocess.run(
                ["bash", str(ROLLOUT), "--plugin", self.STAGE0, "--plugin", self.STAGE1,
                 "--canary", "--canary-stage-size", "1", "--canary-threshold", "0",
                 "--no-rebuild", "--no-sync"],
                env=env, capture_output=True, text=True, cwd=str(REPO),
            )
            log = p.stdout + p.stderr
            # Controls: the scenario really happened as designed.
            self.assertEqual(p.returncode, 4, f"expected halt exit 4, got {p.returncode}\n{log}")
            self.assertIn(f"copied {self.STAGE0} ->", log, f"stage 0 not applied\n{log}")
            self.assertIn("canary: HALTED at stage 1", log, log)

            data = json.loads(reg.read_text())
            for n in (self.STAGE0, self.STAGE1):
                ent = data["plugins"][f"{n}@yukineko"][0]
                ip = Path(ent["installPath"])
                self.assertTrue(ip.is_dir(), f"{n}: registry points at missing dir {ip}")
                has_host = (ip / "bin" / f"{n}-{suffix}").is_file()
                self.assertTrue(
                    has_host,
                    f"{n}: registry -> {ip} (version {ent['version']}) but that dir has "
                    f"no host binary bin/{n}-{suffix}: plugin is registered and execs "
                    f"nothing (dark).\n{log}",
                )


class ConcurrentRolloutIsExcluded(unittest.TestCase):
    """e3366b5c (second half): two rollouts must not run at once. The script
    takes an exclusive mkdir lock at "$CLAUDE_PLUGIN_CACHE/.rollout.lock" and
    fails closed (non-zero, before touching registry/cache) on contention,
    including a STALE lock (holder pid dead): never stolen silently."""

    PLUGIN = "taskprog"  # non-gate crate: no canary requirement

    def _sandbox(self, tmp):
        tmp = Path(tmp)
        home, cache = tmp / "home", tmp / "cache" / "yukineko"
        reg = tmp / "installed_plugins.json"
        home.mkdir()
        d = cache / self.PLUGIN / "0.0.1-prior"
        d.mkdir(parents=True)
        (d / "marker").write_text("prior")
        _write_registry(reg, {self.PLUGIN: ("0.0.1-prior", d)})
        return tmp, home, cache, reg

    def _run(self, home, cache, reg):
        return subprocess.run(
            ["bash", str(ROLLOUT), "--plugin", self.PLUGIN, "--no-rebuild", "--no-sync"],
            env=_env(home, cache, reg), capture_output=True, text=True, cwd=str(REPO),
        )

    @staticmethod
    def _listing(cache):
        return sorted(str(p.relative_to(cache)) for p in cache.rglob("*")
                      if ".rollout.lock" not in p.parts)

    def _make_lock(self, cache, pid):
        lock = cache / ".rollout.lock"
        lock.mkdir()
        (lock / "pid").write_text(f"{pid}\n")
        return lock

    def _assert_refused(self, r, reg, before_reg, cache, before_list, lock, pid):
        log = r.stdout + r.stderr
        self.assertNotEqual(r.returncode, 0, f"rollout ran despite held lock\n{log}")
        self.assertEqual(reg.read_bytes(), before_reg, "registry modified under a held lock")
        self.assertEqual(self._listing(cache), before_list, "cache modified under a held lock")
        self.assertIn(".rollout.lock", r.stderr, f"stderr does not name the lock\n{log}")
        self.assertTrue(lock.is_dir(), "another session's lock was removed/stolen")
        return log

    def test_live_holder_blocks_second_rollout(self):
        with tempfile.TemporaryDirectory() as t:
            tmp, home, cache, reg = self._sandbox(t)
            pid = os.getpid()  # a live process
            lock = self._make_lock(cache, pid)
            before_reg, before_list = reg.read_bytes(), self._listing(cache)
            r = self._run(home, cache, reg)
            log = self._assert_refused(r, reg, before_reg, cache, before_list, lock, pid)
            self.assertIn(str(pid), r.stderr, f"stderr does not name the holder pid\n{log}")

    def test_stale_lock_is_not_silently_stolen(self):
        with tempfile.TemporaryDirectory() as t:
            tmp, home, cache, reg = self._sandbox(t)
            dead = subprocess.Popen([sys.executable, "-c", "pass"])
            dead.wait()  # reaped: pid is dead
            lock = self._make_lock(cache, dead.pid)
            before_reg, before_list = reg.read_bytes(), self._listing(cache)
            r = self._run(home, cache, reg)
            log = self._assert_refused(r, reg, before_reg, cache, before_list, lock, dead.pid)
            self.assertIn("rm -r", log, f"stale-lock message must say how to remove it\n{log}")

    def test_control_without_lock_proceeds_and_releases(self):
        """Control: 'always refuse' would pass the tests above. Without a lock
        the run must get past the lock stage and not leave the lock behind."""
        with tempfile.TemporaryDirectory() as t:
            tmp, home, cache, reg = self._sandbox(t)
            r = self._run(home, cache, reg)
            log = r.stdout + r.stderr
            self.assertNotIn(".rollout.lock", log, f"refused with no lock present\n{log}")
            self.assertIn(f"copied {self.PLUGIN} ->", log, f"rollout did not proceed\n{log}")
            self.assertFalse((cache / ".rollout.lock").exists(), "lock left behind after run")


if __name__ == "__main__":
    unittest.main()
