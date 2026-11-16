#!/usr/bin/env python3
"""RED (2026-11-17): stop-verify-worktree blocks a session that ran only
READ-ONLY Bash commands in main, because another process changed main during
one of those calls and guard-maintree-bash.py recorded the change in THIS
session's write record.

Observed (session 1e000416, 2026-11-17; verbatim Stop-hook text):
  "Do not stop yet: main-tree paths THIS session wrote are still uncommitted
  in /Users/yuki/src/harness."
  naming `.backlog/tasks.toml` (written by another session's commits a7857dbbe /
  aa18dda2c "backlog: file 7768aeaf"), and on the next Stop
  `.claude-plugin/marketplace.json`, `Cargo.lock`, 29 paths under
  `crates/condukt/**`, `docs/specs/condukt.md` -- a session that never touched
  condukt. `git status` on main was empty right after each block. The session
  had run from the main cwd only read-only commands (`git status`, `git log`,
  `git -C <wt> ...`, blastguard probes, `tdd skip`, `blastguard relay dismiss`).

Path of the misattribution (read from the code, not assumed):
  * scripts/guard-maintree-bash.py post(): `touched = changed_paths(before,
    after)` (every path whose status/hash differs between the Pre and Post
    snapshots of main) is appended to this session's write record via
    deny_ledger.record_writes, whatever the command text was. The module doc
    states it: "Changes by ANOTHER session in main between a main-cwd call's
    Pre and Post are REFUSED ... and recorded in THIS session's write record
    (over-attribution, the restrictive side)".
  * scripts/deny_ledger.py writes_stop_check(): blocks on every recorded path
    that is dirty now; the 2026-11-15 mtime filter (mtime > max(epoch) + 30 s)
    does not help, because the foreign write happened INSIDE the call window,
    so its mtime is before the record epoch taken at Post.
  * stop-verify-worktree.py does NOT use harness-core attribution.rs
    (read_only_stage; backlog bd07c598). It is pure Python over that record.

What this test pins: a session whose main-cwd Bash call was `git status`
(read-only) must not be blocked at Stop for a path another process dirtied
during that call. The control: a call whose command really writes the path
must still block.

DOES NOT PROVE: which discriminator is right (command-text read-only
classification, a per-process write observation, ...); that the observed
blocks came from exactly this window shape (the window timing of the real
foreign writes is UNVERIFIED -- only the record's existence and the Stop text
were observed). Over-attribution was the documented, intended restrictive
side; making this GREEN is a visible contract change of guard-maintree-bash.py.

    python3 -m pytest scripts/test_stop_verify_worktree_readonly_concurrent_20261117.py
"""
from __future__ import annotations

import json
import os
import subprocess
import sys
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))
from test_guard_maintree_bash_observe import GIT_ID, ObserveGuardBase, _clean_env, git  # noqa: E402

STOP = Path(__file__).resolve().parent / "stop-verify-worktree.py"


class ReadOnlySessionConcurrentForeignWrite(ObserveGuardBase):
    SESSION = "sess-stopverify-ro-0001"

    def _stop(self) -> subprocess.CompletedProcess:
        env = _clean_env()
        env["HOME"] = str(self.tmp / "home")  # same sandboxed write record as the guard
        env["STOP_VERIFY_WORKTREE_STATE_DIR"] = str(self.tmp / "svw-state")
        payload = {"hook_event_name": "Stop", "session_id": self.SESSION,
                   "cwd": str(self.main), "stop_hook_active": False}
        return subprocess.run([sys.executable, str(STOP)], input=json.dumps(payload),
                              cwd=str(self.main), env=env, capture_output=True,
                              text=True, timeout=60)

    def _record_file(self) -> Path:
        return self.tmp / "home" / ".claude" / "state" / "maintree-writes" / (
            self.SESSION + ".jsonl")

    def _foreign_write(self) -> None:
        """Another session's in-flight `backlog add` / condukt edit in main."""
        (self.main / ".backlog").mkdir(exist_ok=True)
        (self.main / ".backlog" / "tasks.toml").write_text("rows = 1\n")
        (self.main / "tracked.txt").write_text("v2 by another session\n")

    def test_readonly_call_with_concurrent_foreign_write_does_not_block_stop(self) -> None:
        cmd = "git status"
        tid, _ = self.pre_ok(cmd)
        subprocess.run(["git", "status"], cwd=str(self.main), env=_clean_env(),
                       check=True, capture_output=True)  # the session's own call
        self._foreign_write()                             # another process, same window
        self.post(tid, cmd)
        # anti-vacuity: the guard did put the foreign paths in THIS session's record
        rec = self._record_file()
        self.assertTrue(rec.exists(), "precondition: guard recorded the window's changes")
        self.assertIn("tasks.toml", rec.read_text())
        r = self._stop()
        self.assertEqual(
            r.returncode, 0,
            "a session that ran only `git status` in main was blocked at Stop for "
            f"another process's write: rc={r.returncode} stderr={r.stderr!r}")

    def test_foreign_main_commits_attributed_to_readonly_session(self) -> None:
        """Fixture with ANOTHER session's main commits (user-specified shape).

        1. this session runs `git status` in main; during that call the other
           session writes .backlog/tasks.toml (dirty at Post) -> the guard
           records it in THIS session's write record;
        2. the other session commits it ("backlog: file 7768aeaf", like
           a7857dbbe);
        3. the other session writes tasks.toml again for its second commit
           (like aa18dda2c) and this session Stops inside that write->commit
           window.
        Observed (a): the Stop is blocked naming .backlog/tasks.toml as a path
        "THIS session wrote", and right after, once the foreign commit lands,
        `git status` is empty. This session never wrote the file."""
        cmd = "git status"
        tid, _ = self.pre_ok(cmd)
        subprocess.run(["git", "status"], cwd=str(self.main), env=_clean_env(),
                       check=True, capture_output=True)
        p = self.main / ".backlog" / "tasks.toml"
        p.parent.mkdir(exist_ok=True)
        p.write_text("rows = 1\n")                  # foreign write inside the window
        self.post(tid, cmd)
        git(self.main, "add", "--", ".backlog/tasks.toml")
        git(self.main, *GIT_ID, "commit", "-q", "-m", "backlog: file 7768aeaf")
        self.assertIn("tasks.toml", self._record_file().read_text(),
                      "precondition: the foreign write is in THIS session's record")
        p.write_text("rows = 2\n")                  # foreign second write, not yet committed
        r = self._stop()
        # the foreign session then commits; main is clean right after (observed)
        git(self.main, "add", "--", ".backlog/tasks.toml")
        git(self.main, *GIT_ID, "commit", "-q", "-m", "backlog: file 7768aeaf")
        self.assertEqual(git(self.main, "status", "--porcelain").stdout, "")
        log = git(self.main, "log", "--format=%s").stdout.splitlines()
        self.assertEqual(log.count("backlog: file 7768aeaf"), 2,
                         "fixture: two foreign main commits")
        self.assertEqual(
            r.returncode, 0,
            "the other session's main commits/writes to .backlog/tasks.toml were "
            f"treated as THIS session's write: rc={r.returncode} stderr={r.stderr!r}")

    def test_control_real_write_by_this_call_still_blocks_stop(self) -> None:
        cmd = "echo v2 > tracked.txt"
        tid, _ = self.pre_ok(cmd)
        subprocess.run(["/bin/sh", "-c", cmd], cwd=str(self.main), env=_clean_env(),
                       check=True, capture_output=True)
        self.post(tid, cmd)
        r = self._stop()
        self.assertEqual(r.returncode, 2,
                         f"this call's own uncommitted write must block: {r.stderr!r}")
        self.assertIn("THIS session wrote", r.stderr)
        self.assertIn("tracked.txt", r.stderr)


if __name__ == "__main__":
    import unittest
    unittest.main()
