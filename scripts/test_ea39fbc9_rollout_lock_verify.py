"""Independent verifier tests for backlog ea39fbc9 (rollout lock honoured by the
standalone prune-plugin-cache.py and rebuild-plugins.sh).

Written by a verifier, not the implementer. Everything runs against temp
cache / registry / settings / HOME; a fake `cargo` is first on PATH so no real
build can happen. Never touches ~/.claude.

Covers: (a) end-to-end reentrancy through the REAL rollout-plugins.sh (a copy of
scripts/ in a throwaway git repo, so REPO resolves to the temp repo), plus a
simulated parent-holder; (b) env var naming a live NON-ancestor pid that matches
the lock -> refused; env var not matching the lock -> refused; (c) held lock,
dead pid, no env -> refused; (d) empty pid file -> refused; (e) standalone prune
without a lock runs and leaves no lock; (f) standalone rebuild without a lock
reaches cargo and leaves no lock.
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
ENV_VAR = "HARNESS_ROLLOUT_LOCK_HOLDER_PID"


def dead_pid():
    p = subprocess.Popen(["true"])
    p.wait()
    return p.pid


class Fx:
    def __init__(self, scripts_dir=HERE, repo=None):
        self.root = Path(tempfile.mkdtemp(prefix="ea39v-")).resolve()
        self.scripts = Path(scripts_dir)
        self.repo = Path(repo) if repo else self.root / "repo"
        if not repo:
            pj = self.repo / "crates" / "p" / ".claude-plugin" / "plugin.json"
            pj.parent.mkdir(parents=True)
            pj.write_text(json.dumps({"name": "p", "version": "2.0.0"}))
        self.cache = self.root / "cache" / "yukineko"
        self.stale = self.cache / "p" / "1.0.0"
        self.current = self.cache / "p" / "2.0.0"
        for d in (self.stale, self.current):
            (d / "bin").mkdir(parents=True)
            (d / "bin" / "p").write_text("#!/bin/sh\n")
        self.registry = self.root / "installed_plugins.json"
        self.registry.write_text(json.dumps({"version": 2, "plugins": {"p@yukineko": [
            {"scope": "user", "installPath": str(self.current), "version": "2.0.0",
             "lastUpdated": "2026-01-01T00:00:00.000Z"}]}}))
        self.settings = self.root / "settings.json"
        self.settings.write_text("{}")
        (self.root / "home").mkdir()
        fakebin = self.root / "fakebin"
        fakebin.mkdir()
        self.cargo_log = self.root / "cargo.log"
        cargo = fakebin / "cargo"
        cargo.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = metadata ]; then echo '{\"target_directory\":\"%s\"}'; exit 0; fi\n"
            "echo \"$*\" >> '%s'\nexit 1\n" % (self.root / "target", self.cargo_log))
        cargo.chmod(0o755)
        self.env = dict(os.environ, HOME=str(self.root / "home"),
                        CLAUDE_PLUGIN_CACHE=str(self.cache),
                        CLAUDE_PLUGIN_REGISTRY=str(self.registry),
                        CLAUDE_SETTINGS_JSON=str(self.settings),
                        PLUGIN_CACHE_PROC_LIST_PROBE="echo '1 1 python3'",
                        PATH=f"{fakebin}{os.pathsep}{os.environ['PATH']}")
        self.env.pop(ENV_VAR, None)
        self.lock = self.cache / ".rollout.lock"

    def cleanup(self):
        shutil.rmtree(self.root, True)

    def hold(self, pid_text):
        self.lock.mkdir()
        if pid_text is not None:
            (self.lock / "pid").write_text(pid_text)

    def prune(self, env_extra=None, extra=()):
        env = dict(self.env, **(env_extra or {}))
        return subprocess.run(
            [sys.executable, str(self.scripts / "prune-plugin-cache.py"), "--repo", str(self.repo),
             "--cache", str(self.cache), "--registry", str(self.registry), *extra],
            capture_output=True, text=True, env=env, timeout=120)

    def rebuild(self, env_extra=None):
        env = dict(self.env, **(env_extra or {}))
        return subprocess.run(["bash", str(self.scripts / "rebuild-plugins.sh")],
                              capture_output=True, text=True, env=env, cwd=str(REPO), timeout=120)

    def cargo_calls(self):
        return self.cargo_log.read_text().splitlines() if self.cargo_log.exists() else []


class Base(unittest.TestCase):
    scripts_dir = HERE

    def setUp(self):
        self.fx = Fx(self.scripts_dir)
        self.addCleanup(self.fx.cleanup)

    def assert_refused(self, r, what):
        out = r.stdout + r.stderr
        self.assertEqual(r.returncode, 6, f"{what}: expected exit 6\n{out}")
        self.assertTrue(self.fx.stale.exists(), f"{what}: stale dir was deleted\n{out}")
        self.assertEqual(self.fx.cargo_calls(), [], f"{what}: cargo was called\n{out}")
        self.assertTrue(self.fx.lock.is_dir(), f"{what}: someone else's lock was removed\n{out}")


class RefusalTests(Base):
    def _both(self, env_extra=None):
        return self.fx.prune(env_extra), self.fx.rebuild(env_extra)

    def test_b_env_names_live_nonancestor_pid_matching_lock_is_refused(self):
        sib = subprocess.Popen(["sleep", "60"])  # live, sibling of the child, not an ancestor
        self.addCleanup(sib.kill)
        self.addCleanup(sib.wait)
        self.fx.hold(f"{sib.pid}\n")
        for name, run in (("prune", self.fx.prune), ("rebuild", self.fx.rebuild)):
            r = run({ENV_VAR: str(sib.pid)})
            self.assert_refused(r, f"{name} with non-ancestor holder")
            self.assertEqual((self.fx.lock / "pid").read_text(), f"{sib.pid}\n")

    def test_b2_env_not_matching_lock_pid_is_refused_even_if_ancestor(self):
        self.fx.hold(f"{os.getpid()}\n")  # lock names the test process (ancestor of children)
        other = subprocess.Popen(["sleep", "60"])
        self.addCleanup(other.kill)
        self.addCleanup(other.wait)
        for name, run in (("prune", self.fx.prune), ("rebuild", self.fx.rebuild)):
            r = run({ENV_VAR: str(other.pid)})
            self.assert_refused(r, f"{name} with mismatching env")

    def test_b3_env_set_but_no_lock_dir_is_refused(self):
        for name, run in (("prune", self.fx.prune), ("rebuild", self.fx.rebuild)):
            r = run({ENV_VAR: str(os.getpid())})
            out = r.stdout + r.stderr
            self.assertEqual(r.returncode, 6, out)
            self.assertTrue(self.fx.stale.exists(), out)
            self.assertEqual(self.fx.cargo_calls(), [], out)

    def test_c_dead_pid_lock_no_env_is_refused_and_not_stolen(self):
        dp = dead_pid()
        self.fx.hold(f"{dp}\n")
        for name, run in (("prune", self.fx.prune), ("rebuild", self.fx.rebuild)):
            r = run()
            self.assert_refused(r, f"{name} with dead-pid lock")
            self.assertEqual((self.fx.lock / "pid").read_text(), f"{dp}\n")

    def test_c2_live_pid_lock_no_env_is_refused(self):
        self.fx.hold(f"{os.getpid()}\n")
        for name, run in (("prune", self.fx.prune), ("rebuild", self.fx.rebuild)):
            self.assert_refused(run(), f"{name} with live-pid lock")

    def test_d_empty_pid_file_is_refused_with_and_without_env(self):
        self.fx.hold("")
        for env in (None, {ENV_VAR: str(os.getpid())}):
            for name, run in (("prune", self.fx.prune), ("rebuild", self.fx.rebuild)):
                self.assert_refused(run(env), f"{name} empty pid, env={env}")

    def test_d2_missing_pid_file_and_garbage_pid_are_refused(self):
        self.fx.hold(None)
        for name, run in (("prune", self.fx.prune), ("rebuild", self.fx.rebuild)):
            self.assert_refused(run(), f"{name} no pid file")
        (self.fx.lock / "pid").write_text("not-a-pid\n")
        for name, run in (("prune", self.fx.prune), ("rebuild", self.fx.rebuild)):
            self.assert_refused(run(), f"{name} garbage pid")
            self.assert_refused(run({ENV_VAR: "not-a-pid"}), f"{name} garbage pid+env")


class StandaloneTests(Base):
    def test_e_standalone_prune_without_lock_runs_and_leaves_no_lock(self):
        r = self.fx.prune()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertFalse(self.fx.stale.exists(), "stale dir should have been pruned")
        self.assertTrue(self.fx.current.exists())
        self.assertFalse(self.fx.lock.exists(), "prune left its lock behind")

    def test_e2_prune_dry_run_ignores_a_held_lock_and_deletes_nothing(self):
        self.fx.hold(f"{os.getpid()}\n")
        r = self.fx.prune(extra=("--dry-run",))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertTrue(self.fx.stale.exists())
        self.assertTrue(self.fx.lock.is_dir())

    def test_f_standalone_rebuild_without_lock_reaches_cargo_and_leaves_no_lock(self):
        r = self.fx.rebuild()
        out = r.stdout + r.stderr
        self.assertNotEqual(r.returncode, 6, out)  # fake cargo refuses with 1
        self.assertTrue(self.fx.cargo_calls(), "rebuild never reached cargo\n" + out)
        self.assertFalse(self.fx.lock.exists(), "rebuild left its lock behind\n" + out)

    def test_f2_standalone_prune_failure_path_still_releases_lock(self):
        # unreadable registry -> prune keeps everything; whatever it exits with,
        # the lock it took must be gone.
        self.fx.registry.write_text("{not json")
        self.fx.prune()
        self.assertFalse(self.fx.lock.exists())


class ReentrancyTests(Base):
    def _parent_holder(self, child_argv, extra_env=None):
        """A real parent process that takes the lock (mkdir + its OWN pid), exports the
        env var, and spawns the child -> the child's ancestor is the lock holder."""
        code = (
            "import os,subprocess,sys\n"
            "lock=sys.argv[1]\nos.mkdir(lock)\n"
            "open(lock+'/pid','w').write(str(os.getpid())+'\\n')\n"
            "os.environ[%r]=str(os.getpid())\n"
            "r=subprocess.run(sys.argv[2:])\n"
            "sys.exit(r.returncode)\n" % ENV_VAR)
        return subprocess.run([sys.executable, "-c", code, str(self.fx.lock), *child_argv],
                              capture_output=True, text=True, env=dict(self.fx.env, **(extra_env or {})),
                              timeout=120)

    def test_a_simulated_parent_holder_prune_accepted_and_lock_survives(self):
        r = self._parent_holder([sys.executable, str(HERE / "prune-plugin-cache.py"), "--repo", str(self.fx.repo),
                                 "--cache", str(self.fx.cache), "--registry", str(self.fx.registry)])
        out = r.stdout + r.stderr
        self.assertEqual(r.returncode, 0, out)
        self.assertFalse(self.fx.stale.exists(), "accepted child did not prune\n" + out)
        # the child must NOT release a lock it did not take (parent never removes it either)
        self.assertTrue(self.fx.lock.is_dir(), "child released a lock it did not take\n" + out)

    def test_a_simulated_parent_holder_rebuild_accepted_and_lock_survives(self):
        r = self._parent_holder(["bash", str(HERE / "rebuild-plugins.sh")])
        out = r.stdout + r.stderr
        self.assertNotEqual(r.returncode, 6, out)
        self.assertTrue(self.fx.cargo_calls(), "accepted child never reached cargo\n" + out)
        self.assertTrue(self.fx.lock.is_dir(), "child released a lock it did not take\n" + out)

    def test_a_grandchild_via_intermediate_shell_is_accepted(self):
        r = self._parent_holder(["bash", "-c", 'bash "$0"', str(HERE / "rebuild-plugins.sh")])
        out = r.stdout + r.stderr
        self.assertNotEqual(r.returncode, 6, out)
        self.assertTrue(self.fx.cargo_calls(), out)


class RolloutE2E(unittest.TestCase):
    """The REAL rollout-plugins.sh, non-dry-run, in a throwaway git repo whose scripts/
    is a copy of this tree's scripts/ (REPO = dirname($0)/.. so it resolves to the temp
    repo). Cache/registry/settings/HOME are temp; cargo is a fake. Hermetic: the only
    paths touched are under the temp root."""

    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="ea39v-e2e-")).resolve()
        self.addCleanup(shutil.rmtree, self.root, True)
        repo = self.root / "repo"
        shutil.copytree(HERE, repo / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
        (repo / "crates" / "p" / ".claude-plugin").mkdir(parents=True)
        (repo / "crates" / "p" / ".claude-plugin" / "plugin.json").write_text('{"name":"p","version":"3.0.0"}')
        (repo / "crates" / "p" / "Cargo.toml").write_text('[package]\nname="p"\nversion="3.0.0"\n')
        (repo / ".claude-plugin").mkdir()
        (repo / ".claude-plugin" / "marketplace.json").write_text(json.dumps(
            {"name": "yukineko", "owner": {"name": "y"}, "plugins": [
                {"name": "p", "version": "3.0.0",
                 "source": {"source": "git-subdir", "url": "x", "path": "crates/p", "ref": "main"}}]}))
        g = ["git", "-c", "user.email=a@b", "-c", "user.name=t"]
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        subprocess.run(["git", "-C", str(repo), "add", "-A"], check=True)
        subprocess.run([*g, "-C", str(repo), "commit", "-qm", "i"], check=True,
                       env=dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null"))
        self.repo = repo
        self.cache = self.root / "cache" / "yukineko"
        self.old = self.cache / "p" / "1.0.0"
        (self.old / "bin").mkdir(parents=True)
        self.registry = self.root / "reg.json"
        self.registry.write_text(json.dumps({"version": 2, "plugins": {"p@yukineko": [
            {"scope": "user", "installPath": str(self.old), "version": "1.0.0",
             "lastUpdated": "2026-01-01T00:00:00.000Z"}]}}))
        (self.root / "settings.json").write_text("{}")
        (self.root / "home").mkdir()
        fb = self.root / "fb"
        fb.mkdir()
        cargo = fb / "cargo"
        cargo.write_text("#!/bin/sh\nif [ \"$1\" = metadata ]; then echo '{\"target_directory\":\"%s\"}'; exit 0; fi\n"
                         "echo \"$*\" >> '%s'\nexit 1\n" % (self.root / "target", self.root / "cargo.log"))
        cargo.chmod(0o755)
        self.env = dict(os.environ, HOME=str(self.root / "home"), CLAUDE_PLUGIN_CACHE=str(self.cache),
                        CLAUDE_PLUGIN_REGISTRY=str(self.registry),
                        CLAUDE_SETTINGS_JSON=str(self.root / "settings.json"),
                        PLUGIN_CACHE_PROC_LIST_PROBE="echo '1 1 python3'",
                        PATH=f"{fb}{os.pathsep}{os.environ['PATH']}")
        self.env.pop(ENV_VAR, None)

    def run_rollout(self, *args):
        return subprocess.run(["bash", str(self.repo / "scripts" / "rollout-plugins.sh"), *args],
                              capture_output=True, text=True, env=self.env, cwd=str(self.repo), timeout=300)

    def test_rebuild_child_is_not_refused_by_its_own_rollouts_lock(self):
        r = self.run_rollout()
        out = r.stdout + r.stderr
        self.assertIn(">>> scripts/rebuild-plugins.sh", out, "E2E did not reach the rebuild child (UNVERIFIED)\n" + out)
        self.assertNotIn("ERROR: rebuild:", out)
        self.assertIn("rollout lock: running under holder pid", out)
        self.assertFalse((self.cache / ".rollout.lock").exists(), "rollout left its lock\n" + out)

    def test_prune_child_is_not_refused_by_its_own_rollouts_lock(self):
        r = self.run_rollout("--no-rebuild", "--no-sync")
        out = r.stdout + r.stderr
        self.assertIn(">>> scripts/prune-plugin-cache.py", out, "E2E did not reach the prune child (UNVERIFIED)\n" + out)
        self.assertNotIn("ERROR: prune:", out)
        self.assertNotIn("prune: exited 6", out)
        self.assertIn("rollout lock: running under holder pid", out)
        self.assertFalse((self.cache / ".rollout.lock").exists(), "rollout left its lock\n" + out)

    def test_standalone_rebuild_refuses_while_a_real_rollout_holds_the_lock(self):
        # a real rollout holder: hold the lock via a sleeping process that owns it
        holder = subprocess.Popen(["sleep", "60"])
        self.addCleanup(holder.kill)
        self.addCleanup(holder.wait)
        (self.cache).mkdir(parents=True, exist_ok=True)
        (self.cache / ".rollout.lock").mkdir()
        (self.cache / ".rollout.lock" / "pid").write_text(f"{holder.pid}\n")
        r = self.run_rollout()  # a second rollout must refuse at ITS acquire, before any child
        self.assertEqual(r.returncode, 6, r.stdout + r.stderr)
        self.assertNotIn(">>> scripts/rebuild-plugins.sh", r.stdout)


if __name__ == "__main__":
    unittest.main()
