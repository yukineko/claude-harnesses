#!/usr/bin/env python3
"""RED (2026-11-15): stop-verify-worktree blocks on a path that THIS session
wrote EARLIER (and that has since been committed) when ANOTHER writer made it
dirty again; the block attributes another writer's dirt to this session.

Observed facts (verbatim; transcript
/Users/yuki/.claude/projects/-Users-yuki-src-harness/6dcca0ad-a40c-4645-956c-9bae0affdc31.jsonl):
  * Stop-hook feedback (user lines 2199 @ 2026-11-15T09:02:34.700Z and 2286 @
    09:06:14.184Z): "Do not stop yet: main-tree paths THIS session wrote are
    still uncommitted in /Users/yuki/src/harness. .backlog/tasks.toml"
  * the session reported `git -C /Users/yuki/src/harness status --short` empty
    right after.
  * ~/.claude/state/maintree-writes/6dcca0ad-....jsonl holds 7 entries for
    /Users/yuki/src/harness/.backlog/tasks.toml, `by: guard-maintree-bash.py`
    (the record is never cleared; scripts/deny_ledger.py:935-946).
  * `git log -- .backlog/tasks.toml` on 2026-11-15: "backlog: file 4242986e"
    at 18:02:22 and 18:02:33 JST (= 09:02:22Z / 09:02:33Z; the first Stop
    block came 1.2 s after the latter), "backlog: file 4270601a" at 18:07:26 JST
    (after the second block). Rows are written to tasks.toml in main and then
    committed, so tasks.toml is transiently dirty between write and commit.
  * deny_ledger.py:928-934 documents the attribution rule: another session
    writing main during a call "is recorded too (over-attribution: the
    restrictive side)". writes_stop_check (deny_ledger.py:1038) looks only at
    the PATH set of the record and `git status` now; entries carry `epoch`
    but it is not used.
NOT observed: which process wrote tasks.toml in the two blocked windows
(this session's own in-flight `backlog add`, or another session's). The
mtime of the transient dirt is gone.

Two readings:
  (1) real defect: the record knows WHEN this session last wrote each path
      (`epoch`, taken at Post, i.e. after the write). A dirty path whose
      mtime is later than every record epoch for it cannot be this session's
      write, so blocking on it is a misattribution (false positive; the
      session cannot fix it, it has nothing to commit).
  (2) intended: over-attribution is the documented restrictive side of a
      path-only record, and the window is the 11 s backlog write->commit.
      Then the fix is to stop auto-merged backlog rows from being recorded
      or to treat them as integration, not this test.
Tell apart: reproduce with two sessions (A runs `backlog add`, B stops during
A's write->commit window) and look at B's record vs. file mtime; if B's
record epoch < file mtime the dirt was not B's -> reading (1).
This test pins reading (1) in its narrowest, mechanical form (mtime strictly
after the last record epoch + 30 s).

DOES NOT PROVE: that the two observed blocks were of this shape (the writer
is UNVERIFIED); that mtime > epoch is the right fix (it is one deterministic
discriminator; a content hash in the record would be another); that blocking
on same-call concurrent writes should stop (that stays over-attributed).
NOTE: existing fixtures in test_stop_verify_worktree.py record epoch=1.0
with a fresh mtime and expect a block; a fix of this kind must update that
fixture (record a realistic epoch), which is a visible contract change.
"""
import json
import os
import subprocess
import sys
import time
from pathlib import Path

import pytest

SCRIPT = Path(__file__).with_name("stop-verify-worktree.py")
SID = "sfp1"


def _git(cwd, *a):
    subprocess.run(["git", "-c", "user.name=t", "-c", "user.email=t@t", *a],
                   cwd=cwd, check=True, capture_output=True)


@pytest.fixture
def env(tmp_path):
    repo = tmp_path / "repo"
    (repo / ".backlog").mkdir(parents=True)
    _git(repo, "init", "-q")
    (repo / ".backlog" / "tasks.toml").write_text("rows = 0\n")
    _git(repo, "add", ".")
    _git(repo, "commit", "-q", "-m", "init")
    home = tmp_path / "home"
    home.mkdir()
    e = dict(os.environ, HOME=str(home),
             STOP_VERIFY_WORKTREE_STATE_DIR=str(tmp_path / "state"),
             TMPDIR=str(tmp_path))
    return repo, e


def _record(repo, e, path, epoch):
    d = Path(e["HOME"]) / ".claude" / "state" / "maintree-writes"
    d.mkdir(parents=True, exist_ok=True)
    with open(d / (SID + ".jsonl"), "a") as f:
        f.write(json.dumps({"v": 1, "kind": "write", "epoch": epoch,
                            "root": os.path.realpath(repo),
                            "path": os.path.realpath(path),
                            "by": "guard-maintree-bash.py"}) + "\n")


def _stop(repo, e):
    return subprocess.run(
        [sys.executable, str(SCRIPT)],
        input=json.dumps({"session_id": SID, "cwd": str(repo)}),
        capture_output=True, text=True, env=e, timeout=60)


def _write_at(p, text, mtime):
    p.write_text(text)
    os.utime(p, (mtime, mtime))


def test_foreign_rewrite_after_my_committed_write_does_not_block(env):
    """This session wrote tasks.toml at T and the row was committed (clean).
    Later another writer dirties it (mtime T+60). Dirt not made by this
    session; the Stop must not block."""
    repo, e = env
    p = repo / ".backlog" / "tasks.toml"
    t = time.time() - 600
    _write_at(p, "rows = 1\n", t)
    _record(repo, e, p, t + 0.5)          # Post-of-call record, just after the write
    _git(repo, "add", ".")
    _git(repo, "commit", "-q", "-m", "backlog: file (this session)")
    assert _stop(repo, e).returncode == 0  # control: clean now, passes at HEAD
    _write_at(p, "rows = 2\n", t + 60)    # another session's in-flight backlog add
    r = _stop(repo, e)
    assert r.returncode == 0, (
        "blocked on dirt whose mtime is after every record epoch for the path "
        f"(not this session's write): {r.stderr!r}")


def test_control_dirt_at_or_before_my_record_epoch_still_blocks(env):
    """Control: this session's own uncommitted write (mtime <= record epoch)
    must keep blocking, so the fix cannot be 'stop blocking recorded paths'."""
    repo, e = env
    p = repo / ".backlog" / "tasks.toml"
    t = time.time() - 600
    _write_at(p, "rows = 1\n", t)
    _record(repo, e, p, t + 0.5)
    r = _stop(repo, e)
    assert r.returncode == 2, f"own uncommitted write did not block: {r.stdout!r} {r.stderr!r}"
