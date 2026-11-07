#!/usr/bin/env python3
"""Background warm-seed + edit-race safety (backlog 675bc422).

Follow-up spec to scripts/test_worktree_target_seed.py (backlog 9d9a429e).
Written by an independent test writer; the implementer must not edit the
asserts to make them pass (CLAUDE.md §2 (a)/(b)).

USER RULING: seeding a real 19G target takes ~92s, so the SessionStart hook
must START the seed in the background and return immediately. That opens a
window in which the agent (or cargo) works in the new worktree WHILE the
seeder is running, which creates two new hazards this file pins:

  * EDIT RACE (fail-open): a tracked file is identical when the seeder decides
    its sync list, then is edited before the seeder applies that file's warm
    mtime. Applying it anyway gives NEW content an OLD mtime, so cargo treats
    it as fresh and runs tests against stale code. After the seeder finishes,
    such a file must not carry the warm mtime, cargo must rebuild it, and the
    binary must reflect the edit.
  * cargo creating target/ while the clone is in flight: the seeder must
    decline, leave cargo's target alone, and touch no mtime.

And: a background failure must not be silent.

──────────────────────────────────────────────────────────────────────────────
INTERFACE THE IMPLEMENTER MUST PROVIDE (in addition to 9d9a429e's)
──────────────────────────────────────────────────────────────────────────────
A. `scripts/session-worktree-init.py`, when it creates a worktree and a seed
   is attempted, returns promptly: the hook process exits AND its stdout/stderr
   reach EOF (no background child may hold the hook's pipes) well before the
   clone completes. The seed continues detached and completes on its own.
B. The hook prints one line matching  `seed log: (/\\S+)`  — an absolute path
   of a file. The background seed appends its outcome there; when the seed is
   over, that file contains the seeder's verdict JSON object (a line with a
   "seeded" key and a "reason"), including for declines and failures.
C. Test seam in `scripts/seed-worktree-target.py`: env var
   `SEED_WORKTREE_TARGET_PAUSE_DIR=<dir>`. When set, at the LAST point before
   the seeder applies any warm mtime — i.e. after it has finished deciding
   which files are identical (and after the clone, if the clone precedes the
   sync) — it creates `<dir>/paused` and blocks until `<dir>/resume` exists
   (bounded wait; on timeout it declines cleanly). Unset = no pause. The seam
   must sit on the real code path, not a test-only branch that skips checks.
D. The seeder (and therefore the hook's background seed) clones via `cp`
   resolved from PATH, so a PATH shim can slow or gate the clone. (True at
   016cdbf8: `("cp", "-c", "-R", "-p", ...)`.)

Tests that need only D run against the current seeder and fail for the real
reason, not for a missing seam (see test docstrings).

Run: python3 -m pytest scripts/test_worktree_target_seed_background.py
"""
import json
import os
import re
import shutil
import subprocess
import sys
import time
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
sys.path.insert(0, str(SCRIPTS))
import test_worktree_target_seed as base  # noqa: E402  (fixture + helpers only)

SEEDER = SCRIPTS / "seed-worktree-target.py"
HOOK = SCRIPTS / "session-worktree-init.py"
BACKLOG_ID = "675bc422"
REL = "lib1/src/lib.rs"
LOG_RE = re.compile(r"seed log: (/\S+)")


def open_defect(backlog_id):
    return unittest.skipUnless(
        os.environ.get("AUDIT_RUN_OPEN"),
        f"backlog {backlog_id}: open defect, remove skip when fixed",
    )


SHIM = """#!/bin/sh
# test shim for cp: signal start, then wait for a go file or a fixed sleep.
if [ -n "$SEED_TEST_GATE" ]; then
  : > "$SEED_TEST_GATE/cp-started"
  i=0
  while [ ! -e "$SEED_TEST_GATE/cp-go" ] && [ $i -lt 1200 ]; do
    sleep 0.1; i=$((i+1))
  done
elif [ -n "$SEED_TEST_CP_SLEEP" ]; then
  sleep "$SEED_TEST_CP_SLEEP"
fi
exec /bin/cp "$@"
"""


def _wait_for(pred, timeout, step=0.1):
    end = time.monotonic() + timeout
    while time.monotonic() < end:
        if pred():
            return True
        time.sleep(step)
    return pred()


class BgFixture(base.Fixture):
    def __init__(self, tc):
        super().__init__(tc)
        self.shimdir = self.base / "shim"
        self.shimdir.mkdir()
        (self.shimdir / "cp").write_text(SHIM)
        os.chmod(self.shimdir / "cp", 0o755)
        self.gate = self.base / "gate"
        self.gate.mkdir()
        self.pause = self.base / "pause"
        self.pause.mkdir()
        # Always release anything a test left blocked, before tmp cleanup.
        tc.addCleanup(self.release)
        self.home = self.base / "home"
        self.home.mkdir()

    def release(self):
        for p in (self.gate / "cp-go", self.pause / "resume"):
            try:
                p.write_text("")
            except OSError:
                pass
        time.sleep(0.5)

    def env(self, **extra):
        e = base._env(**extra)
        e["PATH"] = f"{self.shimdir}{os.pathsep}{e['PATH']}"
        return e

    def start_seeder(self, new, **extra):
        return subprocess.Popen(
            [sys.executable, str(SEEDER), str(self.warm), str(new)],
            env=self.env(**extra), stdout=subprocess.PIPE,
            stderr=subprocess.PIPE, text=True)

    def run_hook(self, sid, **extra):
        t0 = time.monotonic()
        out = subprocess.run(
            (sys.executable, str(HOOK)),
            input=json.dumps({"cwd": str(self.warm), "session_id": sid}),
            capture_output=True, text=True,
            env=self.env(HOME=str(self.home), **extra), timeout=600,
        )
        return out, time.monotonic() - t0


def _verdict(proc, tc):
    out, err = proc.communicate(timeout=300)
    last = (out.strip().splitlines() or ["<no stdout>"])[-1]
    try:
        return json.loads(last)
    except ValueError:
        tc.fail(f"seeder printed no JSON verdict: stdout={out!r} stderr={err!r}")


def _log_verdict(log):
    try:
        text = log.read_text()
    except OSError:
        return None
    for ln in reversed(text.splitlines()):
        try:
            v = json.loads(ln)
        except ValueError:
            continue
        if isinstance(v, dict) and "seeded" in v:
            return v
    return None


def _edit_same_size(path):
    # Same byte length as the "warm" fixture content, fresh mtime.
    text = base.LIB1.format("edit")
    assert len(text) == len(base.LIB1.format("warm"))
    path.write_text(text)


# ───────────────────────── A/B: hook backgrounds the seed ─────────────────────

class HookBackgroundsSeed(unittest.TestCase):
    @open_defect(BACKLOG_ID)
    def test_hook_returns_before_slow_clone_finishes_and_target_appears(self):
        f = BgFixture(self)
        out, elapsed = f.run_hook("bgseed12-0", SEED_TEST_CP_SLEEP="12")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn("A session worktree is ready at", out.stdout)
        self.assertLess(elapsed, 6.0,
                        f"hook (incl. pipe EOF) took {elapsed:.1f}s while the "
                        f"clone sleeps 12s — seed is not backgrounded:\n{out.stdout}")
        wt = f.base / ".repo-worktrees" / "session-bgseed12"
        self.assertTrue(wt.is_dir(), out.stdout)
        shutil.copy2(f.warm / "Cargo.lock", wt / "Cargo.lock")  # untracked
        m = LOG_RE.search(out.stdout)
        self.assertIsNotNone(m, f"no 'seed log: <path>' line:\n{out.stdout}")
        log = Path(m.group(1))
        self.assertTrue(_wait_for(lambda: _log_verdict(log) is not None, 120),
                        f"background seed never recorded a verdict in {log}")
        v = _log_verdict(log)
        self.assertIs(v["seeded"], True, v)
        self.assertTrue((wt / "target" / "debug" / "app").exists())
        compiling, err = base._cargo_build(wt)
        self.assertEqual(compiling, [], err)


class HookSeedFailureIsRecorded(unittest.TestCase):
    @open_defect(BACKLOG_ID)
    def test_background_clone_failure_reason_lands_in_printed_log(self):
        f = BgFixture(self)
        bad = f.warm / "target" / "debug" / "unreadable"
        bad.write_text("x")
        os.chmod(bad, 0)
        out, _ = f.run_hook("bgfail12-0")
        self.assertEqual(out.returncode, 0, out.stderr)
        m = LOG_RE.search(out.stdout)
        self.assertIsNotNone(m, f"no 'seed log: <path>' line:\n{out.stdout}")
        log = Path(m.group(1))
        self.assertTrue(_wait_for(lambda: _log_verdict(log) is not None, 120),
                        f"no verdict recorded in {log}")
        v = _log_verdict(log)
        self.assertIs(v["seeded"], False, v)
        self.assertTrue(str(v.get("reason", "")).strip(), v)
        wt = f.base / ".repo-worktrees" / "session-bgfail12"
        self.assertFalse((wt / "target").exists())


# ───────────────────────────── SAFETY: edit race ─────────────────────────────

class EditRace(unittest.TestCase):
    def _assert_edit_survives(self, f, new, warm_mtime, v):
        self.assertNotEqual(
            os.stat(new / REL).st_mtime_ns, warm_mtime,
            f"edited file carries the warm (older) mtime after seeding: {v}")
        compiling, err = base._cargo_build(new)
        self.assertIn("lib1", compiling, err)
        self.assertEqual(base._run_bin(new), "edit",
                         "binary reflects the warm (stale) source, not the edit")

    @open_defect(BACKLOG_ID)
    def test_edit_during_clone_window_is_not_given_warm_mtime(self):
        """Needs only interface D. Against 016cdbf8 this fails for the REAL
        reason: the sync list is computed before the clone and applied after
        it without re-checking, so the edited file gets the warm mtime."""
        f = BgFixture(self)
        new = f.add_worktree("race-cp")
        shutil.copy2(f.warm / "Cargo.lock", new / "Cargo.lock")
        warm_mtime = os.stat(f.warm / REL).st_mtime_ns
        p = f.start_seeder(new, SEED_TEST_GATE=str(f.gate))
        self.assertTrue(_wait_for((f.gate / "cp-started").exists, 60),
                        "clone never started")
        _edit_same_size(new / REL)
        (f.gate / "cp-go").write_text("")
        v = _verdict(p, self)
        self._assert_edit_survives(f, new, warm_mtime, v)

    @open_defect(BACKLOG_ID)
    def test_edit_at_pause_seam_before_mtime_apply_is_not_given_warm_mtime(self):
        """Needs interface C. Durable regardless of how the implementer
        orders clone vs. planning: the edit lands after the identical-file
        decision is final and before any warm mtime is written."""
        f = BgFixture(self)
        new = f.add_worktree("race-seam")
        shutil.copy2(f.warm / "Cargo.lock", new / "Cargo.lock")
        warm_mtime = os.stat(f.warm / REL).st_mtime_ns
        p = f.start_seeder(new, SEED_WORKTREE_TARGET_PAUSE_DIR=str(f.pause))
        paused = _wait_for((f.pause / "paused").exists, 60)
        if not paused:
            p.kill()
            out, err = p.communicate()
            self.fail("seeder never reached the SEED_WORKTREE_TARGET_PAUSE_DIR "
                      f"seam (interface C): stdout={out!r} stderr={err!r}")
        _edit_same_size(new / REL)
        (f.pause / "resume").write_text("")
        v = _verdict(p, self)
        self._assert_edit_survives(f, new, warm_mtime, v)


# ─────────────── control: cargo creates target/ during the clone ─────────────

class CargoRacesClone(unittest.TestCase):
    def test_cargo_target_created_mid_clone_makes_seeder_decline(self):
        """Needs only interface D; exercises 016cdbf8's renamex_np(EXCL)."""
        f = BgFixture(self)
        new = f.add_worktree("cargo-race")
        shutil.copy2(f.warm / "Cargo.lock", new / "Cargo.lock")
        p = f.start_seeder(new, SEED_TEST_GATE=str(f.gate))
        self.assertTrue(_wait_for((f.gate / "cp-started").exists, 60),
                        "clone never started")
        base._cargo_build(new)  # cargo makes its own target/ now
        cargo_files = sorted(str(q.relative_to(new / "target"))
                             for q in (new / "target").rglob("*"))
        before = base._mtimes(new, base.TRACKED)
        (f.gate / "cp-go").write_text("")
        v = _verdict(p, self)
        self.assertIs(v.get("seeded"), False, v)
        self.assertEqual(p.returncode, 0, v)
        self.assertEqual(base._mtimes(new, base.TRACKED), before,
                         "declined seed touched mtimes")
        after = sorted(str(q.relative_to(new / "target"))
                       for q in (new / "target").rglob("*"))
        self.assertEqual(after, cargo_files, "cargo's target/ was altered")
        leftovers = [q.name for q in new.iterdir()
                     if q.name not in ("target",) and q.name.startswith(".target")]
        self.assertEqual(leftovers, [], "partial clone left behind")
        self.assertEqual(base._run_bin(new), "warm")


if __name__ == "__main__":
    unittest.main()
