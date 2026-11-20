#!/usr/bin/env python3
"""RED (backlog d4a135ba): the rollout mutual-exclusion lock must live at a
fixed path OUTSIDE ~/.claude, not in the plugin cache.

Background: the lock is `$CACHE/.rollout.lock`, inside the marketplace cache dir
that Claude Code garbage-collects at startup (see
test_rollout_lock_outside_managed_cache.py). User ruling 2026-11-20: the new
location is the fixed

    ${XDG_STATE_HOME:-$HOME/.local/state}/claude-harnesses/rollout.lock

(one lock for all caches). A LIVE legacy `$CACHE/.rollout.lock` still blocks
during the transition (fail closed).

Contract asserted here, per contender (rollout-plugins.sh, rebuild-plugins.sh,
prune-plugin-cache.py), by RUNNING them in a sandbox (never by grepping source):
  (a) while the contender holds the lock it exists at
      $XDG_STATE_HOME/claude-harnesses/rollout.lock (and, with XDG_STATE_HOME
      unset, at $HOME/.local/state/claude-harnesses/rollout.lock), with the
      holder pid recorded in ./pid, and the held path is NOT under
      $HOME/.claude (real paths compared);
  (b) fail closed: if the state dir cannot be created (XDG_STATE_HOME is a
      regular file / a read-only dir / $HOME/.local is a regular file with
      XDG_STATE_HOME unset) the contender exits 6 and does not proceed;
  (c) a legacy $CACHE/.rollout.lock whose recorded holder pid is alive makes
      each contender exit 6 (GREEN at base: the legacy location IS the current
      location; it must stay GREEN after the move);
  (d) controls true before and after the fix: with no lock anywhere no
      contender is refused with exit 6; and while one contender holds the lock
      every contender is refused with exit 6.

How a holder is kept inside its critical section (as in the existing RED):
  * rollout-plugins.sh: a `python3` shim on PATH blocks on the registry-validity
    check until ctl/go exists, then exits 1.
  * rebuild-plugins.sh: a `cargo` shim blocks (when RB_HOLD_DIR is set) on its
    first post-lock call, then exits 1.
  * prune-plugin-cache.py: a sitecustomize.py (only on the prune holder's
    PYTHONPATH) blocks the first signal.signal() call, which prune makes right
    after taking the lock, until ctl/go exists.
A contender that gets past the lock either hits a shim (prints SHIM-PAST-LOCK)
or proceeds; none of them can modify anything outside the temp sandbox.

HERMETIC: HOME, XDG dirs, cache and registry are all temp dirs. The real
~/.claude, plugin cache and ~/.local/state are never read or written; cargo is
never really invoked.
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
KINDS = ("rollout", "rebuild", "prune")


def _under(path, parent):
    path, parent = os.path.realpath(path), os.path.realpath(parent)
    return path == parent or path.startswith(parent + os.sep)


class RolloutLockPathOutsideClaudeHome(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory(prefix="rollout-lock-path-")
        tmp = Path(self._tmp.name)
        self.tmp = tmp
        self.home = tmp / "home"
        self.claude_home = self.home / ".claude"
        self.cache = self.claude_home / "plugins" / "cache" / "yukineko"
        self.registry = self.claude_home / "plugins" / "installed_plugins.json"
        self.ctl = tmp / "ctl"
        self.xdg_state = tmp / "xdg-state"
        shim = tmp / "shim"
        pyhook = tmp / "pyhook"
        for d in (self.cache / KNOWN_PLUGIN / "1.0.0", self.ctl, shim, pyhook):
            d.mkdir(parents=True)
        self.registry.write_text(json.dumps({"version": 2, "plugins": {}}))

        real_py = shutil.which("python3") or sys.executable
        (shim / "python3").write_text(
            f"""#!/usr/bin/env bash
hit=0
for a in "$@"; do [ "$a" = "{self.registry}" ] && hit=1; done
if [ "$hit" = 1 ]; then
  if mkdir "{self.ctl}/held_rollout" 2>/dev/null; then
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
            f"""#!/usr/bin/env bash
if [ -n "${{RB_HOLD_DIR:-}}" ] && mkdir "$RB_HOLD_DIR/held_rebuild" 2>/dev/null; then
  while [ ! -e "$RB_HOLD_DIR/go" ]; do sleep 0.1; done
  exit 1
fi
echo "SHIM-PAST-LOCK (cargo $1 reached)" >&2
exit 1
"""
        )
        for f in ("python3", "cargo"):
            (shim / f).chmod(0o755)
        (pyhook / "sitecustomize.py").write_text(
            f"""import os, signal, time
_d = os.environ.get("PRUNE_HOLD_DIR")
if _d:
    _real = signal.signal
    def _hold(signum, handler):
        if signum == signal.SIGINT and not os.path.exists(os.path.join(_d, "held_prune")):
            os.mkdir(os.path.join(_d, "held_prune"))
            while not os.path.exists(os.path.join(_d, "go")):
                time.sleep(0.1)
        return _real(signum, handler)
    signal.signal = _hold
"""
        )
        self.pyhook = pyhook

        env = {
            k: v
            for k, v in os.environ.items()
            if k not in ("HARNESS_ROLLOUT_LOCK_HOLDER_PID", "XDG_STATE_HOME", "PYTHONPATH", "RB_HOLD_DIR", "PRUNE_HOLD_DIR")
        }
        env.update(
            HOME=str(self.home),
            XDG_RUNTIME_DIR=str(tmp / "xdg-run"),
            XDG_STATE_HOME=str(self.xdg_state),
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
        self.holder = None
        self.sleepers = []

    def tearDown(self):
        (self.ctl / "go").touch()
        for s in self.sleepers:
            s.kill()
            s.wait()
        if self.holder is not None:
            try:
                self.holder.wait(timeout=RUN_TIMEOUT_S)
            except subprocess.TimeoutExpired:
                self.holder.send_signal(signal.SIGTERM)
                self.holder.wait(timeout=10)
        # restore perms so TemporaryDirectory can clean up
        for p in (self.xdg_state,):
            try:
                os.chmod(p, 0o700)
            except OSError:
                pass
        self._tmp.cleanup()

    # -- helpers ---------------------------------------------------------
    def argv(self, kind):
        return {
            "rollout": ["bash", str(ROLLOUT), "--no-rebuild", "--no-sync"],
            "rebuild": ["bash", str(REBUILD), "--no-clean", f"--only={KNOWN_PLUGIN}"],
            "prune": [
                sys.executable,
                str(PRUNE),
                "--repo",
                str(REPO),
                "--cache",
                str(self.cache),
                "--registry",
                str(self.registry),
            ],
        }[kind]

    def expected_lock(self, env):
        state = env.get("XDG_STATE_HOME") or str(Path(env["HOME"]) / ".local" / "state")
        return Path(state) / "claude-harnesses" / "rollout.lock"

    def run_contender(self, kind, env=None, pre_go=True):
        if pre_go:
            # a contender that gets past the lock must not block forever on a shim
            (self.ctl / "go").touch()
        return subprocess.run(
            self.argv(kind),
            cwd=REPO,
            env=env or self.env,
            capture_output=True,
            text=True,
            timeout=RUN_TIMEOUT_S,
        )

    def start_holder(self, kind, env=None):
        env = dict(env or self.env)
        if kind == "rebuild":
            env["RB_HOLD_DIR"] = str(self.ctl)
        elif kind == "prune":
            env["PRUNE_HOLD_DIR"] = str(self.ctl)
            env["PYTHONPATH"] = str(self.pyhook)
        self.holder = subprocess.Popen(
            self.argv(kind),
            cwd=REPO,
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            text=True,
        )
        marker = self.ctl / f"held_{kind}"
        deadline = time.monotonic() + HOLD_TIMEOUT_S
        while not marker.exists():
            if self.holder.poll() is not None:
                out, err = self.holder.communicate()
                self.fail(
                    f"holder {kind} exited rc={self.holder.returncode} before reaching its "
                    f"post-lock hold point\\nstdout:\\n{out}\\nstderr:\\n{err}"
                )
            if time.monotonic() > deadline:
                self.fail(f"holder {kind} did not reach its post-lock hold point in time")
            time.sleep(0.1)
        return env

    def assert_refused(self, r, name, why):
        self.assertEqual(
            r.returncode,
            LOCK_REFUSED_EXIT,
            f"{name} was not refused ({why}) rc={r.returncode}\nstdout tail:\n{r.stdout[-800:]}\n"
            f"stderr tail:\n{r.stderr[-1500:]}",
        )
        self.assertNotIn("SHIM-PAST-LOCK", r.stderr, f"{name} got past the lock ({why})")
        # exit 6 must come from the lock refusal itself, not from an unrelated
        # failure: all three contenders print this line only when refusing the lock.
        self.assertIn(
            "Refusing to proceed",
            r.stderr,
            f"{name} exited 6 but not via its lock refusal ({why})\nstderr tail:\n{r.stderr[-1500:]}",
        )
        self.assertNotIn(
            "no current version known",
            r.stderr,
            f"{name} reached prune's fixture-dependent scan ({why}): it got past the lock",
        )

    # -- (a) location while held ----------------------------------------
    def _check_held_location(self, kind, env):
        lock = self.expected_lock(env)
        self.assertTrue(
            lock.is_dir(),
            f"{kind} holder: lock dir not found at {lock} while held "
            f"(legacy {self.cache / '.rollout.lock'} exists: {(self.cache / '.rollout.lock').exists()})",
        )
        pid_file = lock / "pid"
        self.assertTrue(pid_file.is_file(), f"{kind} holder: {pid_file} missing")
        self.assertTrue(pid_file.read_text().strip().isdigit(), f"{kind} holder: {pid_file} holds no pid")
        self.assertFalse(
            _under(lock, self.claude_home),
            f"{kind} holder: held lock {lock} is under {self.claude_home}",
        )

    def test_held_lock_at_xdg_state_home(self):
        for kind in KINDS:
            with self.subTest(contender=kind):
                (self.ctl / "go").unlink(missing_ok=True)
                for h in self.ctl.glob("held_*"):
                    h.rmdir()
                env = self.start_holder(kind)
                try:
                    self._check_held_location(kind, env)
                finally:
                    (self.ctl / "go").touch()
                    self.holder.wait(timeout=RUN_TIMEOUT_S)
                    self.holder = None

    def test_held_lock_at_home_local_state_when_xdg_unset(self):
        env0 = {k: v for k, v in self.env.items() if k != "XDG_STATE_HOME"}
        for kind in KINDS:
            with self.subTest(contender=kind):
                (self.ctl / "go").unlink(missing_ok=True)
                for h in self.ctl.glob("held_*"):
                    h.rmdir()
                env = self.start_holder(kind, env0)
                try:
                    self._check_held_location(kind, env)
                finally:
                    (self.ctl / "go").touch()
                    self.holder.wait(timeout=RUN_TIMEOUT_S)
                    self.holder = None

    # -- (b) fail closed when the state dir is unusable ------------------
    def _refused_with_env(self, env, why):
        for kind in KINDS:
            with self.subTest(contender=kind, why=why):
                r = self.run_contender(kind, env)
                self.assert_refused(r, kind, why)

    def test_xdg_state_home_is_a_regular_file_refuses(self):
        f = self.tmp / "state-is-a-file"
        f.write_text("not a dir")
        env = dict(self.env, XDG_STATE_HOME=str(f))
        self._refused_with_env(env, "XDG_STATE_HOME is a regular file")

    @unittest.skipIf(os.geteuid() == 0, "root ignores directory permission bits")
    def test_xdg_state_home_read_only_refuses(self):
        os.chmod(self.xdg_state, 0o500)
        self._refused_with_env(self.env, "XDG_STATE_HOME is a read-only dir")

    def test_home_local_is_a_regular_file_refuses_when_xdg_unset(self):
        (self.home / ".local").write_text("not a dir")
        env = {k: v for k, v in self.env.items() if k != "XDG_STATE_HOME"}
        self._refused_with_env(env, "XDG_STATE_HOME unset and $HOME/.local is a regular file")

    # -- (c) live legacy lock still blocks -------------------------------
    def test_live_legacy_lock_refuses(self):
        sleeper = subprocess.Popen(["sleep", "300"])
        self.sleepers.append(sleeper)
        legacy = self.cache / ".rollout.lock"
        legacy.mkdir()
        (legacy / "pid").write_text(f"{sleeper.pid}\n")
        self._refused_with_env(self.env, f"live legacy lock {legacy} (pid {sleeper.pid})")

    # -- (d) controls (true before and after the fix) ---------------------
    def test_control_no_lock_anywhere_not_refused(self):
        for kind in KINDS:
            with self.subTest(contender=kind):
                r = self.run_contender(kind)
                self.assertNotEqual(
                    r.returncode,
                    LOCK_REFUSED_EXIT,
                    f"{kind} refused with exit 6 although no lock exists anywhere\nstderr tail:\n{r.stderr[-1500:]}",
                )
                self.assertNotIn("holds", r.stderr.split("Refusing")[0] if "Refusing" in r.stderr else "")

    def test_control_held_lock_refuses_every_contender(self):
        for holder_kind in KINDS:
            (self.ctl / "go").unlink(missing_ok=True)
            for h in self.ctl.glob("held_*"):
                h.rmdir()
            self.start_holder(holder_kind)
            try:
                for kind in KINDS:
                    with self.subTest(holder=holder_kind, contender=kind):
                        r = self.run_contender(kind, pre_go=False)
                        self.assert_refused(r, kind, f"{holder_kind} holds the lock")
            finally:
                (self.ctl / "go").touch()
                self.holder.wait(timeout=RUN_TIMEOUT_S)
                self.holder = None


if __name__ == "__main__":
    unittest.main()
