#!/usr/bin/env python3
"""RED: `backlog merge-driver --install` writes the invoking binary's own path
(`std::env::current_exe()`, crates/backlog/src/merge_driver.rs) into
`merge.backlog.driver`. When that binary lives in a versioned plugin-cache dir
(`~/.claude/plugins/cache/yukineko/backlog/<ver>/bin/...`), the next rollout
supersedes <ver> and scripts/prune-plugin-cache.py deletes the dir. From then
on every merge that touches `.backlog/tasks.toml` fails the driver and goes UU,
even when the two sides' changes do not overlap.

Observed 2026-11-13: merging origin/main left `.backlog/tasks.toml` UU;
`merge.backlog.driver` named `.../backlog/0.3.86/bin/backlog-linux-x86_64`,
which no longer existed. The same driver run by hand from 0.3.111 merged the
three stages cleanly (exit 0).

Fixture: build the backlog binary, copy it into a throwaway
`cache/yukineko/backlog/0.0.1/bin/`, run `merge-driver --install` FROM that
copy in a throwaway repo, then delete the 0.0.1 dir (what prune does to a
superseded version). Ours adds one task and theirs adds a different one --
a clean id-level merge. The defect: the merge does not complete.

What this test does NOT prove: it does not run prune-plugin-cache.py itself
(the dir removal is simulated), and it does not pin HOW the fix works (a
version-independent driver path, or prune/rollout re-pointing the config,
would both turn it GREEN).

Repro:  python3 -m pytest --runxfail scripts/test_backlog_merge_driver_pinned_path_20261113.py
"""
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import pytest

REPO_ROOT = Path(__file__).resolve().parent.parent


def task(tid):
    return (f'[[task]]\nid = "{tid}"\ntitle = "task {tid}"\nproject = "/repo"\n'
            f'tags = []\nstatus = "pending"\nnotes = ""\ncreated_at = 1\n'
            f'updated_at = 1\nweight = 0.0\n')


def env():
    e = dict(os.environ)
    for k in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY",
              "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR", "GIT_PREFIX"):
        e.pop(k, None)
    e.update(GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
             GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
             GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com")
    return e


def git(repo, *args, check=True):
    r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, env=env())
    if check and r.returncode != 0:
        raise AssertionError(f"git {args} failed: {r.stdout}{r.stderr}")
    return r


def build_backlog() -> Path:
    r = subprocess.run(["cargo", "build", "-q", "-p", "backlog", "--message-format=short"],
                       cwd=str(REPO_ROOT), capture_output=True, text=True)
    if r.returncode != 0:
        raise AssertionError(f"cargo build -p backlog failed: {r.stdout}{r.stderr}")
    target = Path(os.environ.get("CARGO_TARGET_DIR", REPO_ROOT / "target"))
    exe = target / "debug" / "backlog"
    if not exe.is_file():
        raise AssertionError(f"built backlog binary not found at {exe}")
    return exe


class PinnedDriverPath(unittest.TestCase):
    @pytest.mark.xfail(strict=True, reason="backlog: merge-driver --install pins a versioned "
                       "plugin-cache path that prune later deletes (RED 2026-11-13)")
    def test_store_merge_survives_removal_of_the_installing_version_dir(self):
        exe = build_backlog()
        with tempfile.TemporaryDirectory() as t:
            tmp = Path(t).resolve()
            ver_dir = tmp / "cache" / "yukineko" / "backlog" / "0.0.1"
            (ver_dir / "bin").mkdir(parents=True)
            cached = ver_dir / "bin" / "backlog-linux-x86_64"
            shutil.copy2(exe, cached)

            repo = tmp / "repo"
            (repo / ".backlog").mkdir(parents=True)
            git(repo, "init", "-q", "-b", "main")
            git(repo, "config", "core.hooksPath", "/dev/null")
            r = subprocess.run([str(cached), "merge-driver", "--install"], cwd=str(repo),
                               capture_output=True, text=True, env=env())
            self.assertEqual(r.returncode, 0, f"install failed: {r.stdout}{r.stderr}")
            (repo / ".gitattributes").write_text(
                ".backlog/tasks.toml merge=backlog\n.backlog/tasks.done.toml merge=backlog\n")
            store = repo / ".backlog" / "tasks.toml"
            store.write_text(task("aaaa0001"))
            (repo / ".backlog" / "tasks.done.toml").write_text("")
            git(repo, "add", ".gitattributes", ".backlog/tasks.toml", ".backlog/tasks.done.toml")
            git(repo, "commit", "-q", "-m", "base")
            git(repo, "checkout", "-q", "-b", "theirs")
            store.write_text(task("aaaa0001") + "\n" + task("cccc0003"))
            git(repo, "commit", "-q", "-am", "theirs adds C")
            git(repo, "checkout", "-q", "main")
            store.write_text(task("aaaa0001") + "\n" + task("bbbb0002"))
            git(repo, "commit", "-q", "-am", "ours adds B")

            # What prune-plugin-cache.py does to a superseded version dir.
            shutil.rmtree(ver_dir)

            m = git(repo, "merge", "--no-ff", "--no-edit", "theirs", check=False)
            self.assertEqual(
                m.returncode, 0,
                "a non-overlapping task-store merge failed once the version dir that ran "
                "`merge-driver --install` was removed; driver = "
                f"{git(repo, 'config', '--get', 'merge.backlog.driver').stdout.strip()}\n"
                f"{m.stdout}{m.stderr}")
            text = store.read_text()
            for tid in ("aaaa0001", "bbbb0002", "cccc0003"):
                self.assertIn(tid, text)


if __name__ == "__main__":
    unittest.main()
