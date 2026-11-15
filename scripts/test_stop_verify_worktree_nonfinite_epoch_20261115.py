#!/usr/bin/env python3
"""RED (2026-11-15): stop-verify-worktree ALLOWS the stop when a recorded,
still-dirty path has a record entry whose `epoch` is NaN or -Infinity.

Observation (independent verifier finding on commit 03a066401, cherry-picked
here; verbatim from scripts/deny_ledger.py at that commit):
  * `_valid_write` accepts the epoch with
    `isinstance(obj.get("epoch"), (int, float))` -- float('nan') and
    float('-inf') are floats, and `json.loads` parses the bare literals
    `NaN` / `-Infinity` into them, so such a line passes validation.
  * writes_stop_check folds epochs with
    `max(path_max_epoch.get(folded, -float('inf')), epoch)`; `max(-inf, nan)`
    is `-inf`, so the per-path max is -inf (also for a lone -Infinity entry).
  * then `if mtime > max_epoch + 30: continue` -> True for any mtime, the
    dirty path is skipped and the stop is allowed.
The task's done_criteria: "an unreadable mtime or epoch stays restrictive
(block)". A non-finite epoch is an unreadable epoch -> must block.

Tests run the real hook (scripts/stop-verify-worktree.py) as a subprocess with
a raw-text jsonl record so the literals appear unquoted, exactly as json.dumps
would emit them for float('nan') / float('-inf').

DOES NOT PROVE: that any producer in the repo actually writes a non-finite
epoch (the recorders use time.time(); the record is a file another process or
a corrupted write can supply); that mixed finite+non-finite records are
handled for the right reason (at 03a066401 `max(finite, nan)` happens to
return the finite value, so the mixed tests below pass now -- they pin the
restrictive outcome, not the mechanism); +Infinity / huge finite epochs
(those make the comparison False -> block, the restrictive side) are not
exercised here.
"""
import json
import os
import subprocess
import sys
import tempfile
import time
import unittest
from pathlib import Path

import pytest

SCRIPT = Path(__file__).with_name("stop-verify-worktree.py")
SID = "snf1"
RED = "RED 2026-11-15: non-finite epoch skips dirty path"


def _git(cwd, *a):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a],
                   cwd=cwd, check=True, capture_output=True)


class NonFiniteEpoch(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory()
        tmp = Path(self._td.name)
        self.repo = tmp / "repo"
        (self.repo / ".backlog").mkdir(parents=True)
        _git(self.repo, "init", "-q")
        self.p = self.repo / ".backlog" / "tasks.toml"
        self.p.write_text("rows = 0\n")
        _git(self.repo, "add", ".")
        _git(self.repo, "commit", "-q", "-m", "init")
        home = tmp / "home"
        home.mkdir()
        self.env = dict(os.environ, HOME=str(home),
                        STOP_VERIFY_WORKTREE_STATE_DIR=str(tmp / "state"),
                        TMPDIR=str(tmp))
        # this session's own uncommitted write, 10 minutes ago
        self.t = time.time() - 600
        self.p.write_text("rows = 1\n")
        os.utime(self.p, (self.t, self.t))

    def tearDown(self):
        self._td.cleanup()

    def _record(self, epoch_text):
        """Append one record line with `epoch_text` spliced in RAW (unquoted)."""
        d = Path(self.env["HOME"]) / ".claude" / "state" / "maintree-writes"
        d.mkdir(parents=True, exist_ok=True)
        root = json.dumps(os.path.realpath(self.repo))
        path = json.dumps(os.path.realpath(self.p))
        line = ('{"v": 1, "kind": "write", "epoch": %s, "root": %s, '
                '"path": %s, "by": "guard-maintree-bash.py"}\n'
                % (epoch_text, root, path))
        with open(d / (SID + ".jsonl"), "a") as f:
            f.write(line)

    def _stop(self):
        return subprocess.run(
            [sys.executable, str(SCRIPT)],
            input=json.dumps({"session_id": SID, "cwd": str(self.repo)}),
            capture_output=True, text=True, env=self.env, timeout=60)

    def _assert_blocks(self, what):
        r = self._stop()
        self.assertNotEqual(
            r.returncode, 0,
            f"{what}: dirty recorded path was skipped, stop allowed "
            f"(rc={r.returncode}) stdout={r.stdout!r} stderr={r.stderr!r}")

    def test_control_finite_epoch_at_write_time_blocks(self):
        self._record(repr(self.t + 0.5))
        self._assert_blocks("control finite epoch")

    def test_nan_epoch_blocks(self):
        self._record("NaN")
        self._assert_blocks("epoch NaN")

    def test_neg_infinity_epoch_blocks(self):
        self._record("-Infinity")
        self._assert_blocks("epoch -Infinity")

    def test_finite_then_nan_same_path_blocks(self):
        # passes at 03a066401 (max(finite, nan) == finite); pins the outcome
        self._record(repr(self.t + 0.5))
        self._record("NaN")
        self._assert_blocks("finite then NaN")

    def test_nan_then_finite_same_path_blocks(self):
        # passes at 03a066401 (max(max(-inf, nan), finite) == finite)
        self._record("NaN")
        self._record(repr(self.t + 0.5))
        self._assert_blocks("NaN then finite")


if __name__ == "__main__":
    unittest.main()
