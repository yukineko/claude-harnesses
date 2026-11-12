"""RED: scripts/check-fail-open-mutation.py rewrites the INVOKING working tree in place.

Observed 2026-11-13 from the main working tree: a push ran the pre-push
fail-open mutation check (blocking check 5, `cd "$REPO" && python3
scripts/check-fail-open-mutation.py --crate blastguard`), and guard-maintree
reported "Changed in /Users/yuki/src/harness: crates/blastguard/src/model.rs".
`apply_mutation` does `path = REPO / scenario.file_rel; path.write_text(mutated)`
— the tracked source file of the tree it was invoked from — and leaves it
mutated for the whole `cargo test -p <crate>` (minutes). CLAUDE.md §8 forbids
any non-merge edit of the main working tree, and any concurrent session that
reads, builds or commits that tree in the window sees the fail-open mutant.

This test runs the REAL script against a scratch git repo laid out like this
one (scripts/ + crates/blastguard/src/model.rs holding the scenario anchor),
with a PATH stub `cargo` that records, at the moment the crate tests run,
whether the invoking tree's tracked model.rs differs from HEAD. Asserted:
it does not (the mutation lives somewhere other than the invoking tree's
tracked file).

CONFLICT TO RESOLVE BEFORE FIXING (not decided here): .githooks/pre-push
documents "runs in THIS worktree only (no temp checkout ...)" and "In place: a
check that must run runs in THIS worktree" (user ruling 2026-11-08). The obvious
fix (mutate a copy / temp checkout) contradicts that wording; a human must rule
which invariant wins, or choose a fix that satisfies both.

Controls (not xfail, GREEN at HEAD): the script exits 0 for a caught mutation
and the file is byte-identical afterwards — so the RED is about the window
during the run, not a lost revert.

DOES NOT PROVE: anything about the other six gate scenarios (same code path,
not exercised), nor about real `cargo test` timing.

Run: python3 -m pytest --runxfail scripts/test_fail_open_mutation_mutates_invoking_tree_20261113.py
Without --runxfail the RED reports as xfailed; strict=True turns it into a
failure (XPASS) once fixed, so remove the mark in the fixing commit.
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "check-fail-open-mutation.py"
TARGET_REL = "crates/blastguard/src/model.rs"
ANCHOR = "            Decision::Ask(reason) => Decision::Deny(reason),\n"

# test-weakening-justified: new RED for an unfixed defect (backlog pending,
# 2026-11-13); xfail(strict) is this repo's python RED convention and turns into
# a failure once the defect is fixed.
OPEN = pytest.mark.xfail(
    strict=True,
    reason="RED: <backlog-pending> 2026-11-13 fail-open mutation check mutates the invoking "
    "tree in place; remove this mark when fixed",
)


def _git(cwd: Path, *args: str) -> str:
    return subprocess.run(
        ["git", *args], cwd=str(cwd), check=True, stdout=subprocess.PIPE, text=True
    ).stdout


class MutationCheckInvokingTree(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="fom-inplace-")).resolve()
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.repo = self.tmp / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        (self.repo / "crates/blastguard/src").mkdir(parents=True)
        shutil.copy2(SCRIPT, self.repo / "scripts" / SCRIPT.name)
        self.original = "impl Decision {\n    fn hardened(self) -> Self {\n        match self {\n" + ANCHOR + "            d => d,\n        }\n    }\n}\n"
        (self.repo / TARGET_REL).write_text(self.original, encoding="utf-8")
        _git(self.repo, "init", "-q")
        _git(self.repo, "config", "user.email", "t@example.invalid")
        _git(self.repo, "config", "user.name", "t")
        _git(self.repo, "add", "-A")
        _git(self.repo, "commit", "-q", "-m", "init")

        self.bin = self.tmp / "bin"
        self.bin.mkdir()
        self.log = self.tmp / "cargo-saw.txt"
        stub = self.bin / "cargo"
        # At cargo time: record the invoking tree's porcelain status for the
        # target file, then report a semantic test failure (=> "caught").
        stub.write_text(
            "#!/bin/sh\n"
            f'git -C "{self.repo}" status --porcelain -- {TARGET_REL} > "{self.log}"\n'
            'echo "test result: FAILED. 0 passed; 1 failed"\n'
            "exit 101\n",
            encoding="utf-8",
        )
        stub.chmod(0o755)

    def _run(self):
        env = dict(os.environ, PATH=f"{self.bin}{os.pathsep}{os.environ['PATH']}")
        return subprocess.run(
            [sys.executable, str(self.repo / "scripts" / SCRIPT.name), "--crate", "blastguard"],
            cwd=str(self.repo),
            env=env,
            stdout=subprocess.PIPE,
            stderr=subprocess.STDOUT,
            text=True,
            timeout=120,
        )

    @OPEN
    def test_invoking_tree_tracked_file_is_not_mutated_during_the_run(self):
        p = self._run()
        self.assertTrue(self.log.exists(), f"stub cargo never ran:\n{p.stdout}")
        saw = self.log.read_text(encoding="utf-8")
        self.assertEqual(
            saw,
            "",
            f"while `cargo test` ran, the invoking tree's tracked {TARGET_REL} was modified "
            f"(git status --porcelain: {saw!r}); script output:\n{p.stdout}",
        )

    def test_control_caught_mutation_exits_zero_and_reverts(self):
        p = self._run()
        self.assertEqual(p.returncode, 0, p.stdout)
        self.assertIn("caught (RED confirmed): 1", p.stdout)
        self.assertEqual((self.repo / TARGET_REL).read_text(encoding="utf-8"), self.original)
        self.assertEqual(_git(self.repo, "status", "--porcelain"), "")


if __name__ == "__main__":
    unittest.main()
