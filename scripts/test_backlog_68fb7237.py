#!/usr/bin/env python3
"""backlog 68fb7237: a merge of `.backlog/tasks.toml` resolved by taking ONE
side wholesale drops the tasks added on the other side, and nothing refuses
the merge commit.

Observed: merge e672436b kept ^1 wholesale; 10 tasks added on ^2 since the
merge-base vanished from both store files (their GitHub issues stayed open,
orphaned). Done criteria in the notes: a merge of .backlog/*.toml that drops an
id present on either side since the merge-base is refused (merge driver or
pre-commit check).

The fixture reproduces e672436b's shape in a throwaway repo WITHOUT the merge
driver (as in any clone where `merge.backlog.driver` is unset, where git falls
back to a text merge): base has A, ours adds B, theirs adds C, the text merge
conflicts, and the conflict is resolved with `git checkout --ours`. It then
runs the gate the repo's pre-commit / pre-merge-commit hook runs on the task
store (scripts/check-backlog-store.py) against that merge state.

Control: the same merge resolved as the union (A, B, C) passes, so the gate
runs and accepts a correct resolution.

Repro:  python3 -m pytest --runxfail scripts/test_backlog_68fb7237.py
"""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
GATE = SCRIPTS / "check-backlog-store.py"


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


def conflicted_merge(tmp: Path) -> Path:
    repo = tmp / "repo"
    (repo / ".backlog").mkdir(parents=True)
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "core.hooksPath", "/dev/null")
    store = repo / ".backlog" / "tasks.toml"
    store.write_text(task("aaaa0001"))
    (repo / ".backlog" / "tasks.done.toml").write_text("")
    git(repo, "add", ".backlog/tasks.toml", ".backlog/tasks.done.toml")
    git(repo, "commit", "-q", "-m", "base")
    git(repo, "checkout", "-q", "-b", "theirs")
    store.write_text(task("aaaa0001") + "\n" + task("cccc0003"))
    git(repo, "commit", "-q", "-am", "theirs adds C")
    git(repo, "checkout", "-q", "main")
    store.write_text(task("aaaa0001") + "\n" + task("bbbb0002"))
    git(repo, "commit", "-q", "-am", "ours adds B")
    r = git(repo, "merge", "--no-ff", "theirs", check=False)
    if r.returncode == 0:
        raise AssertionError(f"fixture: expected a text conflict, merge succeeded: {r.stdout}")
    return repo


def run_gate(repo: Path):
    return subprocess.run([sys.executable, str(GATE)], cwd=str(repo), capture_output=True,
                          text=True, env=env())


class WholesaleSideMerge(unittest.TestCase):
    def test_control_union_resolution_passes(self):
        with tempfile.TemporaryDirectory() as t:
            repo = conflicted_merge(Path(t).resolve())
            (repo / ".backlog" / "tasks.toml").write_text(
                task("aaaa0001") + "\n" + task("bbbb0002") + "\n" + task("cccc0003"))
            git(repo, "add", ".backlog/tasks.toml")
            r = run_gate(repo)
            self.assertEqual(r.returncode, 0, r.stderr)

    def test_ours_wholesale_resolution_dropping_theirs_id_is_refused(self):
        """backlog 68fb7237: open defect (RED observed)."""
        with tempfile.TemporaryDirectory() as t:
            repo = conflicted_merge(Path(t).resolve())
            git(repo, "checkout", "--ours", "--", ".backlog/tasks.toml")
            git(repo, "add", ".backlog/tasks.toml")
            staged = git(repo, "show", ":.backlog/tasks.toml").stdout
            self.assertNotIn("cccc0003", staged, "fixture: ours must lack theirs' id")
            r = run_gate(repo)
            self.assertNotEqual(
                r.returncode, 0,
                "the store gate accepted a merge resolution that dropped id cccc0003, "
                f"added on the other side since the merge-base.\nstderr={r.stderr}")


if __name__ == "__main__":
    unittest.main()
