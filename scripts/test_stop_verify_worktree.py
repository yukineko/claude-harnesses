#!/usr/bin/env python3
"""RED tests: stop-verify-worktree must not skip the check on ANOTHER gate's
re-entry (CLAUDE.md 1, reference design: harness_core run_guarded).

ASSUMED CONTRACT (implementer must match):
  * marker directory is taken from env var  STOP_VERIFY_WORKTREE_STATE_DIR
    (the hook creates it if needed; one marker file per session, named by
    session_id, written when THIS gate blocks and removed on a clean run).
  * a re-entry (stop_hook_active=true) is allowed only when this gate's marker
    for the session exists; the allow carries a JSON {"systemMessage": ...}
    on stdout. A re-entry with no session_id, or an unreadable/unwritable
    marker dir, is also an allow, but WITH a systemMessage (never silent).
"""
import json
import os
import subprocess
import sys
from pathlib import Path

import pytest

SCRIPT = Path(__file__).with_name("stop-verify-worktree.py")


def _git(cwd, *a):
    subprocess.run(["git", *a], cwd=cwd, check=True, capture_output=True)


@pytest.fixture
def env(tmp_path):
    repo = tmp_path / "repo"
    repo.mkdir()
    _git(repo, "init", "-q")
    _git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit",
         "-q", "--allow-empty", "-m", "init")
    home = tmp_path / "home"
    home.mkdir()
    state = tmp_path / "state"
    e = dict(os.environ, HOME=str(home), STOP_VERIFY_WORKTREE_STATE_DIR=str(state),
             TMPDIR=str(tmp_path))
    return repo, state, e


def dirty(repo, e=None, sid=None, name="dirty.txt"):
    """Make a violation. RE-SCOPED by user ruling 2026-11-08 ("donegateその
    ものが他のworktreeを見たり検証したりテストしたりすることは禁止とする。..."):
    the Stop gate now blocks only on dirt in THIS session's own write record, so
    the dirty path is also entered in that record
    ($HOME/.claude/state/maintree-writes/<sid>.jsonl, as the hooks write it).
    Without e/sid it is unattributed (another session's) dirt."""
    p = repo / name
    p.write_text("x")
    if e is not None and sid is not None:
        d = Path(e["HOME"]) / ".claude" / "state" / "maintree-writes"
        d.mkdir(parents=True, exist_ok=True)
        with open(d / (sid + ".jsonl"), "a") as f:
            f.write(json.dumps({"v": 1, "kind": "write", "epoch": 1.0,
                                "root": os.path.realpath(repo),
                                "path": os.path.realpath(p), "by": "test"}) + "\n")


def clean(repo):
    (repo / "dirty.txt").write_text("")
    _git(repo, "add", "dirty.txt")
    _git(repo, "-c", "user.name=t", "-c", "user.email=t@t", "commit",
         "-q", "-m", "tidy")


def run(repo, e, **payload):
    payload.setdefault("cwd", str(repo))
    return subprocess.run([sys.executable, str(SCRIPT)], input=json.dumps(payload),
                          capture_output=True, text=True, env=e, timeout=60)


def has_sysmsg(r):
    try:
        return bool(json.loads(r.stdout).get("systemMessage"))
    except Exception:
        return False


def test_1_first_stop_with_violation_blocks(env):
    repo, _, e = env
    dirty(repo, e, "s1")
    assert run(repo, e, session_id="s1").returncode == 2


def test_1b_unattributed_dirt_does_not_block(env):
    """Control for test_1 (user ruling 2026-11-08): the same dirt, absent from
    this session's record, is another session's and does not block."""
    repo, _, e = env
    dirty(repo)
    assert run(repo, e, session_id="s1b").returncode == 0


def test_2_reentry_without_own_marker_still_blocks(env):
    repo, _, e = env
    dirty(repo, e, "s2")
    r = run(repo, e, session_id="s2", stop_hook_active=True)
    assert r.returncode == 2, f"another gate's re-entry skipped the check: {r.stdout!r}"


def test_3_reentry_with_own_marker_bounded_allow_with_message(env):
    repo, _, e = env
    dirty(repo, e, "s3")
    assert run(repo, e, session_id="s3").returncode == 2  # writes marker
    r = run(repo, e, session_id="s3", stop_hook_active=True)
    assert r.returncode == 0
    assert has_sysmsg(r), r.stdout


def test_4_clean_run_clears_marker(env):
    repo, _, e = env
    dirty(repo, e, "s4")
    assert run(repo, e, session_id="s4").returncode == 2
    clean(repo)
    assert run(repo, e, session_id="s4").returncode == 0
    dirty(repo, e, "s4", name="dirty2.txt")
    r = run(repo, e, session_id="s4", stop_hook_active=True)
    assert r.returncode == 2, "stale marker survived a clean run"


def test_5a_reentry_missing_session_id_explicit_allow_with_message(env):
    repo, _, e = env
    dirty(repo)
    r = run(repo, e, stop_hook_active=True)
    assert r.returncode == 0 and has_sysmsg(r), (r.returncode, r.stdout)


def test_5b_reentry_unusable_marker_dir_explicit_allow_with_message(env, tmp_path):
    repo, _, e = env
    blocker = tmp_path / "file-not-dir"
    blocker.write_text("x")
    e = dict(e, STOP_VERIFY_WORKTREE_STATE_DIR=str(blocker / "sub"))
    dirty(repo)
    r = run(repo, e, session_id="s5", stop_hook_active=True)
    assert r.returncode == 0 and has_sysmsg(r), (r.returncode, r.stdout)


if __name__ == "__main__":
    import unittest
    unittest.main()
