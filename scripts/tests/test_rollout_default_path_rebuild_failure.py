"""Regression for backlog 06ec3503: on the default (non-canary) path of
scripts/rollout-plugins.sh, a FAILED rebuild must not leave the shared registry
(installed_plugins.json) pointing at the fresh version dir, which at that moment
holds only the launcher and no host binary (the plugin goes dark).

Required behaviour when the rebuild fails on the default path:
  * the script exits non-zero, AND
  * every registry entry this run repointed is back at its previous
    installPath AND version (or was never repointed before a successful rebuild),
  * an entry this run CREATED (the plugin had no entry before) is gone again,
    mirroring the canary path's rollback of a newly introduced plugin
    (registry_patch: "A triple whose version AND target are both empty REMOVES").

How this runs the script without running a real rollout
-------------------------------------------------------
The script is copied VERBATIM (byte for byte, read from the current tree) into a
throwaway git repo inside a temp dir and executed there as a whole file:
argument parsing, plan(), copy_plugin_dir() (real rsync), registry_patch(), the
mkdir lock and its EXIT trap, and the real top-level default-path tail all run
unmodified. `cd "$(dirname "$0")/.."` makes the throwaway repo its $REPO.

Only the external collaborator SCRIPTS that $REPO provides are fakes:
  scripts/rebuild-plugins.sh     -> succeeds (seeds a host binary) or fails
  scripts/prune-plugin-cache.py  -> exit 0
  scripts/check-plugin-rollout.py-> exit 0
HOME, CLAUDE_PLUGIN_CACHE and CLAUDE_PLUGIN_REGISTRY all point into the temp
dir, so ~/.claude is never read or written. Plugin names are not GATE_CRATES,
so the default (non-canary) path is the one taken. Stdlib only.
"""
import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REAL_SCRIPT = HERE.parent / "rollout-plugins.sh"
BASH = "/bin/bash" if os.path.exists("/bin/bash") else shutil.which("bash")
OWNER = "yukineko"
HOST_SUFFIX = "darwin-arm64"

REBUILD_OK = f"""#!/bin/bash
# fake rebuild: seed a host binary next to every launcher in the cache
set -eu
for launcher in "$CLAUDE_PLUGIN_CACHE"/*/*/bin/*; do
  case "$launcher" in *-{HOST_SUFFIX}) continue ;; esac
  [ -f "$launcher" ] || continue
  printf 'binary\\n' > "$launcher-{HOST_SUFFIX}"
done
echo "fake rebuild-plugins.sh: ok $*"
"""

REBUILD_FAIL = """#!/bin/bash
echo "fake rebuild-plugins.sh: cargo build failed $*" >&2
exit 1
"""


class Sandbox:
    def __init__(self):
        self.root = Path(tempfile.mkdtemp(prefix="rollout-06ec3503-")).resolve()
        self.home = self.root / "home"
        self.home.mkdir()
        self.repo = self.root / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / ".claude-plugin").mkdir()
        self.cache = self.root / "cache" / OWNER
        self.cache.mkdir(parents=True)
        self.registry = self.root / "installed_plugins.json"
        shutil.copyfile(REAL_SCRIPT, self.repo / "scripts" / "rollout-plugins.sh")
        (self.repo / "scripts" / "rollout-plugins.sh").chmod(0o755)
        self._write("scripts/prune-plugin-cache.py", "import sys\nsys.exit(0)\n")
        self._write("scripts/check-plugin-rollout.py", "import sys\nsys.exit(0)\n")

    def _write(self, rel, body, mode=0o644):
        p = self.repo / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(body)
        p.chmod(mode)
        return p

    def add_plugin(self, name, new_version, old_version=None):
        """A repo crate at new_version; if old_version, a LIVE deployed old dir
        (launcher + host binary) plus a registry entry pointing at it."""
        self._write(f"crates/{name}/Cargo.toml", f'[package]\nname = "{name}"\nversion = "{new_version}"\n')
        self._write(f"crates/{name}/.claude-plugin/plugin.json",
                    json.dumps({"name": name, "version": new_version}))
        self._write(f"crates/{name}/bin/{name}", "#!/bin/sh\nexit 0\n", 0o755)
        old_dir = None
        if old_version:
            old_dir = self.cache / name / old_version
            (old_dir / "bin").mkdir(parents=True)
            (old_dir / "bin" / name).write_text("#!/bin/sh\nexit 0\n")
            (old_dir / "bin" / f"{name}-{HOST_SUFFIX}").write_text("binary\n")
        return old_dir

    def finish(self, plugins, registry_entries, rebuild_ok):
        self._write(".claude-plugin/marketplace.json", json.dumps({"name": OWNER, "plugins": [
            {"name": n, "version": v, "source": {"source": "local", "path": f"crates/{n}"}}
            for n, v in plugins]}))
        self.registry.write_text(json.dumps({"version": 2, "plugins": registry_entries}, indent=2))
        self._write("scripts/rebuild-plugins.sh", REBUILD_OK if rebuild_ok else REBUILD_FAIL, 0o755)
        git = ["git", "-C", str(self.repo), "-c", "core.hooksPath=/dev/null",
               "-c", "user.name=t", "-c", "user.email=t@example.invalid"]
        subprocess.run(git + ["init", "-q"], check=True, capture_output=True)
        subprocess.run(git + ["add", "-A"], check=True, capture_output=True)
        subprocess.run(git + ["commit", "-q", "-m", "fixture"], check=True, capture_output=True)

    def run(self, *args):
        env = dict(os.environ, HOME=str(self.home), CLAUDE_PLUGIN_CACHE=str(self.cache),
                   CLAUDE_PLUGIN_REGISTRY=str(self.registry))
        for k in ("OVERWATCH_BIN", "OVERWATCH_CANARY_SINCE"):
            env.pop(k, None)
        return subprocess.run([BASH, str(self.repo / "scripts" / "rollout-plugins.sh"), *args],
                              capture_output=True, text=True, env=env, cwd=str(self.repo),
                              timeout=300)

    def registry_plugins(self):
        return json.loads(self.registry.read_text())["plugins"]

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


def entry(path, version):
    return [{"scope": "user", "installPath": str(path), "version": version,
             "installedAt": "2026-01-01T00:00:00.000Z", "lastUpdated": "2026-01-01T00:00:00.000Z",
             "gitCommitSha": "1" * 40}]


class DefaultPathRebuildFailure(unittest.TestCase):
    maxDiff = None

    def setUp(self):
        self.assertTrue(REAL_SCRIPT.is_file(), REAL_SCRIPT)
        self.sb = Sandbox()
        self.addCleanup(self.sb.cleanup)

    def out(self, r):
        return f"rc={r.returncode}\n--- stdout ---\n{r.stdout[-3000:]}\n--- stderr ---\n{r.stderr[-3000:]}"

    def assert_lock_released(self, r):
        self.assertFalse((self.sb.cache / ".rollout.lock").exists(),
                         "the rollout lock was left behind\n" + self.out(r))

    # (1) control ------------------------------------------------------------
    def test_control_successful_rebuild_repoints_registry_and_exits_0(self):
        sb = self.sb
        old_a = sb.add_plugin("alpha", "2.0.0", "1.0.0")
        old_b = sb.add_plugin("beta", "3.1.0", "3.0.0")
        sb.finish([("alpha", "2.0.0"), ("beta", "3.1.0")],
                  {f"alpha@{OWNER}": entry(old_a, "1.0.0"), f"beta@{OWNER}": entry(old_b, "3.0.0")},
                  rebuild_ok=True)
        r = sb.run()
        self.assertEqual(r.returncode, 0, self.out(r))
        self.assertIn("fake rebuild-plugins.sh: ok", r.stdout, "the rebuild step never ran\n" + self.out(r))
        reg = sb.registry_plugins()
        for name, ver in (("alpha", "2.0.0"), ("beta", "3.1.0")):
            e = reg[f"{name}@{OWNER}"][0]
            self.assertEqual(e["installPath"], str(sb.cache / name / ver), self.out(r))
            self.assertEqual(e["version"], ver, self.out(r))
            self.assertTrue((sb.cache / name / ver / "bin" / f"{name}-{HOST_SUFFIX}").is_file())
        self.assert_lock_released(r)

    # (2) single plugin, failed rebuild ---------------------------------------
    def test_failed_rebuild_exits_nonzero_and_restores_old_entry(self):
        sb = self.sb
        old = sb.add_plugin("alpha", "2.0.0", "1.0.0")
        sb.finish([("alpha", "2.0.0")], {f"alpha@{OWNER}": entry(old, "1.0.0")}, rebuild_ok=False)
        r = sb.run()
        self.assertIn("cargo build failed", r.stderr, "the failing rebuild never ran\n" + self.out(r))
        self.assertNotEqual(r.returncode, 0, "rebuild failed but the rollout exited 0\n" + self.out(r))
        new_dir = sb.cache / "alpha" / "2.0.0"
        self.assertFalse((new_dir / "bin" / f"alpha-{HOST_SUFFIX}").exists(),
                         "fixture error: the new dir should hold no host binary")
        e = sb.registry_plugins()[f"alpha@{OWNER}"][0]
        self.assertEqual(e["installPath"], str(old),
                         "rebuild failed but the registry still points alpha at the "
                         f"binaryless dir {e['installPath']}\n" + self.out(r))
        self.assertEqual(e["version"], "1.0.0",
                         f"rebuild failed but the registry still says alpha is {e['version']}\n" + self.out(r))
        self.assert_lock_released(r)

    # (3) two plugins, failed rebuild: BOTH restored --------------------------
    def test_failed_rebuild_restores_every_repointed_entry(self):
        sb = self.sb
        old_a = sb.add_plugin("alpha", "2.0.0", "1.0.0")
        old_b = sb.add_plugin("beta", "3.1.0", "3.0.0")
        sb.finish([("alpha", "2.0.0"), ("beta", "3.1.0")],
                  {f"alpha@{OWNER}": entry(old_a, "1.0.0"), f"beta@{OWNER}": entry(old_b, "3.0.0")},
                  rebuild_ok=False)
        r = sb.run()
        self.assertIn("cargo build failed", r.stderr, "the failing rebuild never ran\n" + self.out(r))
        self.assertNotEqual(r.returncode, 0, "rebuild failed but the rollout exited 0\n" + self.out(r))
        reg = sb.registry_plugins()
        got = {n: (reg[f"{n}@{OWNER}"][0]["installPath"], reg[f"{n}@{OWNER}"][0]["version"])
               for n in ("alpha", "beta")}
        want = {"alpha": (str(old_a), "1.0.0"), "beta": (str(old_b), "3.0.0")}
        self.assertEqual(got, want,
                         "rebuild failed but not every repointed registry entry was restored\n" + self.out(r))
        self.assert_lock_released(r)

    # (4) fresh install (no prior entry), failed rebuild ----------------------
    # DESIGN ASSUMPTION: "restore the previous state" of an entry that did not
    # exist means removing it, as the canary rollback already does for a
    # newly introduced plugin. Leaving it registered at a launcher-only dir is
    # the dark state this ticket is about.
    def test_failed_rebuild_removes_entry_created_by_this_run(self):
        sb = self.sb
        old_a = sb.add_plugin("alpha", "2.0.0", "1.0.0")
        sb.add_plugin("gamma", "0.1.0")  # never installed before
        sb.finish([("alpha", "2.0.0"), ("gamma", "0.1.0")],
                  {f"alpha@{OWNER}": entry(old_a, "1.0.0")}, rebuild_ok=False)
        r = sb.run()
        self.assertIn("cargo build failed", r.stderr, "the failing rebuild never ran\n" + self.out(r))
        self.assertNotEqual(r.returncode, 0, "rebuild failed but the rollout exited 0\n" + self.out(r))
        reg = sb.registry_plugins()
        self.assertNotIn(f"gamma@{OWNER}", reg,
                         "rebuild failed but a plugin first registered by this run is left "
                         f"registered at a binaryless dir: {reg.get(f'gamma@{OWNER}')}\n" + self.out(r))
        a = reg[f"alpha@{OWNER}"][0]
        self.assertEqual((a["installPath"], a["version"]), (str(old_a), "1.0.0"), self.out(r))
        self.assert_lock_released(r)


if __name__ == "__main__":
    unittest.main()
