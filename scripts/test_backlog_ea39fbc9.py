"""Repro for backlog ea39fbc9: standalone prune-plugin-cache.py and rebuild-plugins.sh do
not honour the rollout lock (`<cache>/.rollout.lock`, held by a LIVE pid).

rollout-plugins.sh takes the lock so read-registry -> copy -> repoint -> prune is
exclusive (backlog e3366b5c). Run on their own, the pruner deletes version dirs and the
rebuild swaps cache binaries while another rollout holds the lock, i.e. mid-way between
its copy and its registry repoint.

Everything runs against a temp cache / registry / settings.json. rebuild-plugins.sh is
run with a fake `cargo` first on PATH that answers `metadata` and refuses (exit 1,
recorded) every other subcommand, so no real build or `cargo clean` can happen; its
cache refresh is never reached. The lock holder pid is this test process (alive).
"""
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent


class Fixture:
    def __init__(self):
        self.root = Path(tempfile.mkdtemp(prefix="ea39fbc9-")).resolve()
        self.repo = self.root / "repo"
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
            {"scope": "user", "installPath": str(self.current), "version": "2.0.0"}]}}))
        self.settings = self.root / "settings.json"
        self.settings.write_text("{}")
        self.env = dict(os.environ, HOME=str(self.root / "home"),
                        CLAUDE_PLUGIN_CACHE=str(self.cache),
                        CLAUDE_PLUGIN_REGISTRY=str(self.registry),
                        CLAUDE_SETTINGS_JSON=str(self.settings))
        (self.root / "home").mkdir()

    def hold_lock(self):
        lock = self.cache / ".rollout.lock"
        lock.mkdir()
        (lock / "pid").write_text(f"{os.getpid()}\n")

    def prune(self):
        return subprocess.run(
            [sys.executable, str(HERE / "prune-plugin-cache.py"), "--repo", str(self.repo),
             "--cache", str(self.cache), "--registry", str(self.registry)],
            capture_output=True, text=True, env=self.env, timeout=120)

    def rebuild(self):
        fakebin = self.root / "fakebin"
        fakebin.mkdir(exist_ok=True)
        self.cargo_log = self.root / "cargo.log"
        cargo = fakebin / "cargo"
        cargo.write_text(
            "#!/bin/sh\n"
            "if [ \"$1\" = metadata ]; then echo '{\"target_directory\":\"%s\"}'; exit 0; fi\n"
            "echo \"$*\" >> '%s'\nexit 1\n" % (self.root / "target", self.cargo_log))
        cargo.chmod(0o755)
        env = dict(self.env, PATH=f"{fakebin}{os.pathsep}{self.env['PATH']}")
        return subprocess.run(["bash", str(HERE / "rebuild-plugins.sh")], capture_output=True,
                              text=True, env=env, cwd=str(REPO), timeout=120)

    def cargo_mutations(self):
        return self.cargo_log.read_text().split("\n") if self.cargo_log.exists() else []


class BacklogEa39fbc9(unittest.TestCase):
    def setUp(self):
        self.fx = Fixture()
        self.addCleanup(shutil.rmtree, self.fx.root, True)

    def test_control_unlocked_prune_removes_the_stale_dir(self):
        r = self.fx.prune()
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertFalse(self.fx.stale.exists())
        self.assertTrue(self.fx.current.exists())

    @unittest.expectedFailure  # backlog ea39fbc9: open defect, remove when fixed
    def test_prune_refuses_while_a_live_rollout_holds_the_lock(self):
        self.fx.hold_lock()
        r = self.fx.prune()
        self.assertTrue(self.fx.stale.exists(), "pruned a dir while another rollout held the lock:\n" + r.stdout)
        self.assertNotEqual(r.returncode, 0)

    def test_control_unlocked_rebuild_reaches_cargo(self):
        r = self.fx.rebuild()
        self.assertNotEqual(r.returncode, 0)  # the fake cargo refuses
        self.assertTrue(self.fx.cargo_mutations(), r.stdout + r.stderr)

    @unittest.expectedFailure  # backlog ea39fbc9: open defect, remove when fixed
    def test_rebuild_refuses_before_building_while_a_live_rollout_holds_the_lock(self):
        self.fx.hold_lock()
        r = self.fx.rebuild()
        self.assertEqual(self.fx.cargo_mutations(), [],
                         "rebuild went on to cargo while the lock was held:\n" + r.stdout[-400:])
        self.assertNotEqual(r.returncode, 0)


if __name__ == "__main__":
    unittest.main()
