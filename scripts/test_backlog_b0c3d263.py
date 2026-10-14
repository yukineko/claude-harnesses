#!/usr/bin/env python3
"""backlog b0c3d263: check-closure-evidence.py blocks every closure record the
backlog CLI writes, because the CLI records `closure.green.runner` as a runner
LABEL (`cargo-test` / `pytest` / `script`, crates/backlog/src/evidence.rs
Runner::label) while the gate requires it to equal the command's first word
(`cargo` / `python3` / `bash`).

The test builds a throwaway git repo with a real F->P pair (prod.py wrong at
red, fixed at green; a committed pytest test), stages the row as `done` with a
closure in the exact shape the CLI writes (copied from a real CLI-written row
in .backlog/tasks.done.toml: runner "pytest", cmd "python3 -m pytest ..."),
and runs the gate script at its boundary with cwd = that repo.

The control stages the identical closure with runner "python3": the gate
accepts it, so the runner field alone is what blocks.

Repro:  python3 -m pytest --runxfail scripts/test_backlog_b0c3d263.py
"""
import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
GATE = SCRIPTS / "check-closure-evidence.py"
TID = "b0c3d263fixture"


def git(repo, *args):
    r = subprocess.run(["git", "-C", str(repo), *args], capture_output=True, text=True,
                       env=_clean_env())
    if r.returncode != 0:
        raise AssertionError(f"git {args} failed: {r.stderr}")
    return r.stdout.strip()


def _clean_env():
    env = dict(os.environ)
    for k in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "GIT_OBJECT_DIRECTORY",
              "GIT_ALTERNATE_OBJECT_DIRECTORIES", "GIT_COMMON_DIR", "GIT_PREFIX"):
        env.pop(k, None)
    env.update(GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.com",
               GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.com")
    return env


def pending_store():
    return f'[[task]]\nid = "{TID}"\ntitle = "fixture"\nstatus = "pending"\n'


def done_store(runner, red, green):
    return (
        f'[[task]]\nid = "{TID}"\ntitle = "fixture"\nstatus = "done"\n\n'
        "[task.closure]\nreason = \"fixed\"\n\n"
        "[task.closure.green]\n"
        f'runner = "{runner}"\n'
        'cmd = "python3 -m pytest tests/test_prod.py"\n'
        "exit = 0\npassed = 1\n"
        f'rev = "{green}"\n'
        "observed_at = 1791024512\n"
        'output_digest = "sha256:0000000000000000000000000000000000000000000000000000000000000000"\n'
        'excerpt = "1 passed"\n\n'
        "[task.closure.red]\n"
        f'rev = "{red}"\n'
        'exit = 1\nkind = "behavioural"\n'
    )


def build_repo(tmp: Path, runner: str) -> Path:
    repo = tmp / "repo"
    (repo / ".backlog").mkdir(parents=True)
    (repo / "tests").mkdir()
    git(repo, "init", "-q", "-b", "main")
    git(repo, "config", "core.hooksPath", "/dev/null")
    (repo / "prod.py").write_text("def f():\n    return 1\n")
    (repo / "tests" / "test_prod.py").write_text(
        "import sys, pathlib\n"
        "sys.path.insert(0, str(pathlib.Path(__file__).resolve().parent.parent))\n"
        "import prod\n\n\ndef test_f():\n    assert prod.f() == 2\n")
    (repo / ".backlog" / "tasks.toml").write_text(pending_store())
    (repo / ".backlog" / "tasks.done.toml").write_text("")
    git(repo, "add", "prod.py", "tests/test_prod.py", ".backlog/tasks.toml",
        ".backlog/tasks.done.toml")
    git(repo, "commit", "-q", "-m", "red")
    red = git(repo, "rev-parse", "HEAD")
    (repo / "prod.py").write_text("def f():\n    return 2\n")
    git(repo, "add", "prod.py")
    git(repo, "commit", "-q", "-m", "green")
    green = git(repo, "rev-parse", "HEAD")
    (repo / ".backlog" / "tasks.toml").write_text(done_store(runner, red, green))
    git(repo, "add", ".backlog/tasks.toml")
    return repo


def run_gate(repo: Path):
    return subprocess.run([sys.executable, str(GATE)], cwd=str(repo), capture_output=True,
                          text=True, env=_clean_env(), timeout=600)


class ClosureRunnerLabel(unittest.TestCase):
    def test_control_first_word_runner_is_accepted(self):
        """Control: same F->P record with runner = the cmd's first word passes,
        so the fixture's evidence is otherwise sound."""
        with tempfile.TemporaryDirectory() as t:
            r = run_gate(build_repo(Path(t).resolve(), "python3"))
            self.assertEqual(r.returncode, 0, f"stdout={r.stdout}\nstderr={r.stderr}")

    @unittest.expectedFailure
    def test_cli_written_runner_label_is_accepted(self):
        """backlog b0c3d263: open defect (RED observed). The CLI writes
        runner = "pytest" for `python3 -m pytest ...`; the gate must accept the
        closure the CLI itself produces."""
        with tempfile.TemporaryDirectory() as t:
            r = run_gate(build_repo(Path(t).resolve(), "pytest"))
            self.assertEqual(
                r.returncode, 0,
                f"gate blocked a CLI-shaped closure.\nstdout={r.stdout}\nstderr={r.stderr}")


if __name__ == "__main__":
    unittest.main()
