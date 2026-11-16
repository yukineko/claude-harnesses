#!/usr/bin/env python3
"""RED repro: the plugin rollout lock lives inside the marketplace cache dir,
which Claude Code itself garbage-collects.

Observed 2026-11-16 (strace of `claude -p ...`): at startup Claude Code
unlinks/rmdirs unknown directories directly under
~/.claude/plugins/cache/<marketplace>/ (a probe dir `.probe-lock-*/pid` was
removed; a plain file at the same level survived), independent of
marketplace autoUpdate. rollout-plugins.sh takes its mutual-exclusion lock as
`mkdir "$CACHE/.rollout.lock"` in exactly that dir (rebuild-plugins.sh and
prune-plugin-cache.py honour the same path). A real
`rollout-plugins.sh --plugin blastguard --canary` run lost its lock mid-run
while another Claude Code session started, and a second rollout could then
have acquired it (backlog e3366b5c / ea39fbc9: concurrent rollouts prune each
other's freshly registered dirs).

Contract asserted here (location-agnostic: no specific new path is encoded):
  1. while a rollout holds the lock, nothing new exists under the marketplace
     cache dir it was given (the lock is not stored there);
  2. after simulating Claude Code's GC of the marketplace cache dir (every
     directory directly under it that is not a known plugin dir is removed),
     the lock is still held: a second rollout-plugins.sh, a standalone
     rebuild-plugins.sh and a standalone prune-plugin-cache.py are each
     refused with exit 6.

How the holder is kept inside its critical section: a `python3` shim on PATH
blocks the first invocation that receives the registry path (rollout's
post-lock "is the registry valid JSON" check) until the test releases it, then
exits 1 so the holder stops without touching anything. Any later invocation
with the registry path (a contender that got past the lock) exits 1 printing
SHIM-PAST-LOCK. A `cargo` shim does the same for rebuild-plugins.sh, whose
first post-lock action is `cargo metadata`.

HERMETIC: HOME, XDG dirs, cache and registry are all temp dirs. The real
~/.claude is never read or written; cargo is never really invoked.
"""
import json
import os
import shutil
import signal
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
REPO = HERE.parent
ROLLOUT = HERE / "rollout-plugins.sh"
REBUILD = HERE / "rebuild-plugins.sh"
PRUNE = HERE / "prune-plugin-cache.py"

KNOWN_PLUGIN = "fakeplug"
LOCK_REFUSED_EXIT = 6
HOLD_TIMEOUT_S = 60
RUN_TIMEOUT_S = 60


def _tree(root):
    return sorted(str(p.relative_to(root)) for p in Path(root).rglob("*"))


class RolloutLockOutsideManagedCache(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="rollout-lock-loc-")
        tmp = Path(self._tmp.name)
        self.home = tmp / "home"
        self.cache = self.home / ".claude" / "plugins" / "cache" / "yukineko"
        self.registry = self.home / ".claude" / "plugins" / "installed_plugins.json"
        self.ctl = tmp / "ctl"
        shim = tmp / "shim"
        for d in (self.cache / KNOWN_PLUGIN / "1.0.0", self.ctl, shim):
            d.mkdir(parents=True)
        self.registry.write_text(json.dumps({"version": 2, "plugins": {}}))

        real_py = shutil.which("python3") or sys.executable
        (shim / "python3").write_text(
            f"""#!/usr/bin/env bash
hit=0
for a in "$@"; do [ "$a" = "{self.registry}" ] && hit=1; done
if [ "$hit" = 1 ]; then
  if mkdir "{self.ctl}/held" 2>/dev/null; then
    while [ ! -e "{self.ctl}/go" ]; do sleep 0.1; done
    exit 1
  fi
  echo "SHIM-PAST-LOCK (python3 registry check reached)" >&2
  exit 1
fi
exec "{real_py}" "$@"
"""
        )
        (shim / "cargo").write_text(
            '#!/usr/bin/env bash\necho "SHIM-PAST-LOCK (cargo $1 reached)" >&2\nexit 1\n'
        )
        for f in ("python3", "cargo"):
            (shim / f).chmod(0o755)

        env = {k: v for k, v in os.environ.items() if k != "HARNESS_ROLLOUT_LOCK_HOLDER_PID"}
        env.update(
            HOME=str(self.home),
            XDG_RUNTIME_DIR=str(tmp / "xdg-run"),
            XDG_STATE_HOME=str(tmp / "xdg-state"),
            XDG_CACHE_HOME=str(tmp / "xdg-cache"),
            XDG_CONFIG_HOME=str(tmp / "xdg-config"),
            XDG_DATA_HOME=str(tmp / "xdg-data"),
            CLAUDE_PLUGIN_CACHE=str(self.cache),
            CLAUDE_PLUGIN_REGISTRY=str(self.registry),
            PATH=f"{shim}{os.pathsep}{os.environ.get('PATH', '')}",
        )
        for k in ("XDG_RUNTIME_DIR", "XDG_STATE_HOME", "XDG_CACHE_HOME", "XDG_CONFIG_HOME", "XDG_DATA_HOME"):
            Path(env[k]).mkdir(mode=0o700)
        self.env = env
        self.baseline = _tree(self.cache)
        self.holder = None

    def tearDown(self):
        (self.ctl / "go").touch()
        if self.holder is not None:
            try:
                self.holder.wait(timeout=RUN_TIMEOUT_S)
            except subprocess.TimeoutExpired:
                self.holder.send_signal(signal.SIGTERM)
                self.holder.wait(timeout=10)
        self._tmp.cleanup()

    def _start_holder(self):
        self.holder = subprocess.Popen(
            ["bash", str(ROLLOUT), "--no-rebuild", "--no-sync"],
            cwd=REPO,
            env=self.env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        deadline = time.monotonic() + HOLD_TIMEOUT_S
        while not (self.ctl / "held").exists():
            if self.holder.poll() is not None:
                out, err = self.holder.communicate()
                self.fail(
                    f"holder rollout exited rc={self.holder.returncode} before reaching its "
                    f"post-lock registry check\nstdout:\n{out}\nstderr:\n{err}"
                )
            if time.monotonic() > deadline:
                self.fail("holder rollout did not reach its post-lock registry check in time")
            time.sleep(0.1)

    def _claude_code_gc(self):
        """What Claude Code was observed to do at startup: remove every
        directory directly under the marketplace cache dir that is not a
        known plugin dir."""
        removed = []
        for entry in self.cache.iterdir():
            if entry.is_dir() and not entry.is_symlink() and entry.name != KNOWN_PLUGIN:
                shutil.rmtree(entry)
                removed.append(entry.name)
        return removed

    def test_lock_is_not_created_under_marketplace_cache_dir(self):
        self._start_holder()
        created = sorted(set(_tree(self.cache)) - set(self.baseline))
        self.assertEqual(
            created,
            [],
            f"while a rollout holds its lock it created {created} under the marketplace "
            f"cache dir {self.cache}, which Claude Code garbage-collects at startup",
        )

    def _assert_contenders_refused(self, removed):
        contenders = {
            "rollout-plugins.sh": ["bash", str(ROLLOUT), "--no-rebuild", "--no-sync"],
            "rebuild-plugins.sh": ["bash", str(REBUILD), "--no-clean", f"--only={KNOWN_PLUGIN}"],
            "prune-plugin-cache.py": [
                sys.executable,
                str(PRUNE),
                "--repo",
                str(REPO),
                "--cache",
                str(self.cache),
                "--registry",
                str(self.registry),
            ],
        }
        for name, argv in contenders.items():
            with self.subTest(contender=name):
                r = subprocess.run(
                    argv,
                    cwd=REPO,
                    env=self.env,
                    capture_output=True,
                    text=True,
                    timeout=RUN_TIMEOUT_S,
                )
                self.assertEqual(
                    r.returncode,
                    LOCK_REFUSED_EXIT,
                    f"{name} was not refused while another rollout holds the lock "
                    f"(simulated Claude Code cache GC removed {removed} from {self.cache}) "
                    f"(rc={r.returncode})\nstderr tail:\n{r.stderr[-1500:]}",
                )

    def test_control_contenders_refused_without_gc(self):
        """Control (expected GREEN at HEAD too): with no GC, the harness does
        observe all three contenders being refused, so a failure of the GC
        test is about the lock's location, not about this fixture."""
        self._start_holder()
        self._assert_contenders_refused(removed=[])

    def test_lock_survives_claude_code_cache_gc(self):
        self._start_holder()
        self._assert_contenders_refused(removed=self._claude_code_gc())


if __name__ == "__main__":
    unittest.main()
