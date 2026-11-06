#!/usr/bin/env python3
"""INTENTIONALLY RED repro (filed 2026-11-07, session c2e3bc17):
scripts/lint-changed-crates.sh and scripts/test-changed-crates.sh ignore
committed-but-unmerged branch work.

Observed: after a subagent COMMITTED its work in a condukt task worktree
(run-20261107-034825-70254), both scripts printed "no crate touched" for that
worktree, because they compute the changed set from `git diff --name-only HEAD`
plus untracked files only. A worktree whose branch carries committed .rs
changes against its merge-base with main, with a clean tree, is reported as
having nothing to check, so donegate's lint-changed / test-changed pass it.

Property asserted (RED tests): in a linked worktree whose branch has a
COMMITTED fmt violation (lint) / failing test (test) in crates/alpha relative to
its merge-base with `main`, and a clean working tree, each script does NOT
report "no crate touched" and exits non-zero.

Controls (pass today, must keep passing): the SAME change left UNCOMMITTED in
the same kind of worktree is caught by each script (exit non-zero), so the
fixture's violation really is one the scripts detect, and a failure of the RED
tests is about the committed-vs-uncommitted scope alone.

Plain RED (no expectedFailure) because `backlog add --repro-test` requires a
behavioural failure at HEAD (same convention as
scripts/test_glossary_parked_marking_20261103.py). Builds throwaway git repos
in a temp dir; never touches the real repository.

    python3 -m pytest -q scripts/test_changed_crates_committed_unmerged_20261107.py
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
LINT = _HERE / "lint-changed-crates.sh"
TEST = _HERE / "test-changed-crates.sh"
CAP = _HERE / "cap-target-dir.sh"

ROOT_MANIFEST = '[workspace]\nresolver = "2"\nmembers = ["crates/alpha", "crates/beta"]\n'
ALPHA_MANIFEST = '[package]\nname = "scratch-alpha"\nversion = "0.0.0"\nedition = "2021"\n'
BETA_MANIFEST = '[package]\nname = "scratch-beta"\nversion = "0.0.0"\nedition = "2021"\n'
CLEAN_LIB = "pub fn answer() -> u32 {\n    42\n}\n"
FMT_VIOLATION = CLEAN_LIB + "\npub fn badly_formatted(   ) ->u32{let x=1;x}\n"
FAILING_TEST = CLEAN_LIB + (
    "\n#[cfg(test)]\nmod tests {\n    #[test]\n    fn red() {\n"
    "        assert_eq!(super::answer(), 43);\n    }\n}\n"
)


def _cargo_available() -> bool:
    if shutil.which("cargo") is None:
        return False
    return all(
        subprocess.run(["cargo", sub, "--version"], capture_output=True).returncode == 0
        for sub in ("fmt", "clippy")
    )


class Repo:
    """A main repo (two-crate workspace, branch `main`) and one linked worktree
    on branch `task`, mirroring a condukt task worktree."""

    def __init__(self) -> None:
        self.base = Path(tempfile.mkdtemp(prefix="changed-unmerged-")).resolve()
        self.main = self.base / "main"
        self.wt = self.base / "task-wt"
        self.env = dict(os.environ)
        for k in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE"):
            self.env.pop(k, None)
        self.env.update(
            LC_ALL="C",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_TERMINAL_PROMPT="0",
            GIT_CEILING_DIRECTORIES=str(self.base),
            GIT_AUTHOR_NAME="t",
            GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t",
            GIT_COMMITTER_EMAIL="t@example.invalid",
        )
        self.env.pop("CARGO_TARGET_DIR", None)
        self.main.mkdir()
        self.git(self.main, "init", "-q", "-b", "main")
        self.git(self.main, "config", "commit.gpgsign", "false")
        for rel, text in {
            ".gitignore": "/target\n",
            "Cargo.toml": ROOT_MANIFEST,
            "crates/alpha/Cargo.toml": ALPHA_MANIFEST,
            "crates/alpha/src/lib.rs": CLEAN_LIB,
            "crates/beta/Cargo.toml": BETA_MANIFEST,
            "crates/beta/src/lib.rs": CLEAN_LIB,
        }.items():
            self.write(self.main, rel, text)
        sd = self.main / "scripts"
        sd.mkdir()
        for s in (LINT, TEST, CAP):
            shutil.copy2(s, sd / s.name)
            (sd / s.name).chmod(0o755)
        self.git(self.main, "add", "-A")
        self.git(self.main, "commit", "-q", "-m", "seed")
        self.git(self.main, "worktree", "add", "-q", "-b", "task", str(self.wt), "main")

    def write(self, root: Path, rel: str, text: str) -> None:
        p = root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def git(self, cwd: Path, *args: str) -> str:
        proc = subprocess.run(["git", *args], cwd=str(cwd), env=self.env,
                              capture_output=True, text=True)
        if proc.returncode != 0:
            raise AssertionError("git %s failed: %s" % (args, proc.stderr))
        return proc.stdout

    def run(self, script: Path) -> subprocess.CompletedProcess:
        return subprocess.run([str(self.wt / "scripts" / script.name)], cwd=str(self.wt),
                              env=self.env, capture_output=True, text=True, timeout=900)

    def cleanup(self) -> None:
        shutil.rmtree(self.base, ignore_errors=True)


@unittest.skipUnless(_cargo_available(), "cargo fmt/clippy not available")
class CommittedUnmergedChangesAreChecked(unittest.TestCase):
    def setUp(self) -> None:
        self.r = Repo()
        self.addCleanup(self.r.cleanup)

    def _commit_in_wt(self, text: str) -> None:
        self.r.write(self.r.wt, "crates/alpha/src/lib.rs", text)
        self.r.git(self.r.wt, "add", "-A")
        self.r.git(self.r.wt, "commit", "-q", "-m", "worker commit")
        status = self.r.git(self.r.wt, "status", "--porcelain")
        self.assertEqual(status, "", "apparatus: the worktree tree must be clean")
        ahead = self.r.git(self.r.wt, "diff", "--name-only", "main...HEAD")
        self.assertIn("crates/alpha/src/lib.rs", ahead,
                      "apparatus: the branch must carry the change vs its merge-base with main")

    def _assert_checked(self, proc: subprocess.CompletedProcess, what: str) -> None:
        out = proc.stdout + proc.stderr
        self.assertFalse(
            proc.returncode == 0 or "no crate touched" in out,
            "%s: committed-but-unmerged crate change on a clean tree was reported as nothing "
            "to check (exit %d)\n--- output ---\n%s" % (what, proc.returncode, out[-1500:]),
        )

    def test_lint_changed_checks_committed_unmerged_fmt_violation(self) -> None:
        self._commit_in_wt(FMT_VIOLATION)
        self._assert_checked(self.r.run(LINT), "lint-changed-crates.sh")

    def test_test_changed_checks_committed_unmerged_failing_test(self) -> None:
        self._commit_in_wt(FAILING_TEST)
        self._assert_checked(self.r.run(TEST), "test-changed-crates.sh")

    # ---- controls: the same change left uncommitted IS caught today ---------
    def test_control_lint_changed_catches_uncommitted_fmt_violation(self) -> None:
        self.r.write(self.r.wt, "crates/alpha/src/lib.rs", FMT_VIOLATION)
        proc = self.r.run(LINT)
        self.assertNotEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    def test_control_test_changed_catches_uncommitted_failing_test(self) -> None:
        self.r.write(self.r.wt, "crates/alpha/src/lib.rs", FAILING_TEST)
        proc = self.r.run(TEST)
        self.assertNotEqual(proc.returncode, 0, proc.stdout + proc.stderr)


if __name__ == "__main__":
    unittest.main()
