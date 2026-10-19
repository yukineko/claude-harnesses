#!/usr/bin/env python3
"""backlog 8231b822: when the task-store merge driver cannot run, git marks
`.backlog/tasks.toml` unmerged but leaves OUR side verbatim with NO conflict
markers. The file parses as valid TOML, so a plain `git add` + commit silently
discards the other side's backlog changes, and nothing refuses that commit.

Observed 2026-10-04: `merge.backlog.driver = 'backlog merge-driver %O %A %B'`
with `backlog` not on git's PATH printed `backlog: command not found`, the
store went UU, and the working file held ours verbatim.

The fixture reproduces that in a throwaway repo: `.gitattributes`
`merge=backlog`, the bare driver config, and a PATH without `backlog`. Ours
adds a task; theirs CHANGES an existing task (status pending -> failed,
new notes) -- a change that is not an id drop, so only content-level loss is
involved. After the driver fails and the file is `git add`ed unchanged, the
gate the pre-commit / pre-merge-commit hook runs on the store
(scripts/check-backlog-store.py) is asked to judge the merge.

Repro:  python3 -m pytest --runxfail scripts/test_backlog_8231b822.py
"""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
GATE = SCRIPTS / "check-backlog-store.py"


def task(tid, status="pending", notes=""):
    return (f'[[task]]\nid = "{tid}"\ntitle = "task {tid}"\nproject = "/repo"\n'
            f'tags = []\nstatus = "{status}"\nnotes = "{notes}"\ncreated_at = 1\n'
            f'updated_at = 1\nweight = 0.0\n')


def env():
    e = dict(os.environ)
    for k in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY",
              "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR", "GIT_PREFIX"):
        e.pop(k, None)
    git_dir = os.path.dirname(subprocess.run(["sh", "-c", "command -v git"], capture_output=True,
                                             text=True).stdout.strip())
    # git's own dir + system dirs only: `backlog` is NOT resolvable.
    e["PATH"] = f"{git_dir}:/usr/bin:/bin"
    e.update(GIT_CONFIG_GLOBAL="/dev/null", GIT_CONFIG_SYSTEM="/dev/null",
             GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
             GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com")
    return e


def git(repo, *args, check=True):
    r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True, env=env())
    if check and r.returncode != 0:
        raise AssertionError(f"git {args} failed: {r.stdout}{r.stderr}")
    return r


def failed_driver_merge(tmp: Path):
    repo = tmp / "repo"
    (repo / ".backlog").mkdir(parents=True)
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "core.hooksPath", "/dev/null")
    git(repo, "config", "merge.backlog.driver", "backlog merge-driver %O %A %B")
    (repo / ".gitattributes").write_text(
        ".backlog/tasks.toml merge=backlog\n.backlog/tasks.done.toml merge=backlog\n")
    store = repo / ".backlog" / "tasks.toml"
    store.write_text(task("aaaa0001"))
    (repo / ".backlog" / "tasks.done.toml").write_text("")
    git(repo, "add", ".gitattributes", ".backlog/tasks.toml", ".backlog/tasks.done.toml")
    git(repo, "commit", "-q", "-m", "base")
    git(repo, "checkout", "-q", "-b", "theirs")
    store.write_text(task("aaaa0001", status="failed", notes="theirs failure note"))
    git(repo, "commit", "-q", "-am", "theirs changes A")
    git(repo, "checkout", "-q", "main")
    ours = task("aaaa0001") + "\n" + task("bbbb0002")
    store.write_text(ours)
    git(repo, "commit", "-q", "-am", "ours adds B")
    r = git(repo, "merge", "--no-ff", "theirs", check=False)
    return repo, r, ours


def run_gate(repo: Path):
    return subprocess.run([sys.executable, str(GATE)], cwd=str(repo), capture_output=True,
                          text=True, env=env())


class FailedDriverMerge(unittest.TestCase):
    def test_fixture_driver_failure_leaves_ours_without_markers(self):
        """Pins the observed git behaviour the defect rests on (GREEN)."""
        with tempfile.TemporaryDirectory() as t:
            repo, r, ours = failed_driver_merge(Path(t).resolve())
            self.assertNotEqual(r.returncode, 0, f"merge must not succeed: {r.stdout}{r.stderr}")
            self.assertIn("not found", r.stdout + r.stderr)
            self.assertEqual((repo / ".backlog" / "tasks.toml").read_text(), ours)

    def test_committing_the_failed_driver_result_is_refused(self):
        """backlog 8231b822: open defect (RED observed). Theirs' change to
        aaaa0001 is silently discarded; the store gate must refuse it."""
        with tempfile.TemporaryDirectory() as t:
            repo, _, _ = failed_driver_merge(Path(t).resolve())
            git(repo, "add", ".backlog/tasks.toml")
            r = run_gate(repo)
            self.assertNotEqual(
                r.returncode, 0,
                "the store gate accepted a merge whose driver never ran, discarding "
                f"theirs' change to aaaa0001.\nstderr={r.stderr}")


if __name__ == "__main__":
    unittest.main()
