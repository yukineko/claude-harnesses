#!/usr/bin/env python3
"""A fail-open mutation run must not silently leave test-written artifacts.

Observed 2026-11-14 (main at 4b02cd3e): pre-push ran check-fail-open-mutation.py
in place. The mutated stuckguard's proptest failed and proptest persisted a
seed file (crates/stuckguard/proptest-regressions/verdict_monotonicity.txt,
untracked). The script reverted the mutated source byte-identically but left
that artifact; the untracked file then made pre-push refuse and dirtied main.

Contract pinned here (either branch is acceptable, silence is not):
  * after the run the tree has no NEW untracked files the run created, OR
  * the script exits non-zero AND names every such file in its output.
Control: an untracked file that existed BEFORE the run is never deleted.

Same style as test_check_fail_open_mutation.py: stdlib unittest, scratch git
repo, run_crate_tests stubbed (the stub plays the part of proptest).
"""
import contextlib
import importlib.util
import io
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open_mutation_artifacts", _HERE / "check-fail-open-mutation.py"
)
cfom = importlib.util.module_from_spec(_SPEC)
# Compiled from source text, not exec_module (stale .pyc risk; backlog 05726f9f).
exec(  # noqa: S102
    compile(_SPEC.loader.get_source(_SPEC.name), _SPEC.origin, "exec", dont_inherit=True),
    cfom.__dict__,
)

ARTIFACT_REL = "crates/fake/proptest-regressions/verdict_monotonicity.txt"


def _run(cmd, cwd):
    return subprocess.run(
        cmd, cwd=str(cwd), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=True
    )


def _make_repo(tmp):
    repo = Path(tmp)
    _run(["git", "init", "-q"], cwd=repo)
    _run(["git", "config", "user.email", "test@example.com"], cwd=repo)
    _run(["git", "config", "user.name", "test"], cwd=repo)
    src = repo / "crates/fake/src/lib.rs"
    src.parent.mkdir(parents=True)
    src.write_text("fn decide() -> bool {\n    true\n}\n", encoding="utf-8")
    _run(["git", "add", "crates/fake/src/lib.rs"], cwd=repo)
    _run(["git", "commit", "-q", "-m", "init"], cwd=repo)
    return repo


def _untracked(repo):
    out = _run(["git", "ls-files", "--others", "--exclude-standard"], cwd=repo).stdout
    return set(out.split())


class MutationRunArtifacts(unittest.TestCase):
    def _drive(self, repo):
        """Run the scenario through main()'s own pipeline with a stub that
        writes an untracked artifact like proptest persistence does.
        Returns (exit_code, captured_output)."""
        scenario = cfom.Scenario(
            crate="fake", file_rel="crates/fake/src/lib.rs", description="d",
            old="    true\n", new="    false\n",
        )

        def stub(crate, timeout=600):
            art = repo / ARTIFACT_REL
            art.parent.mkdir(parents=True, exist_ok=True)
            art.write_text("cc deadbeef # shrinks to input = []\n", encoding="utf-8")
            return 101, "running 1 test\ntest p ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed;\n"

        old_repo, old_run_tests = cfom.REPO, cfom.run_crate_tests
        cfom.REPO = repo
        cfom.run_crate_tests = stub
        buf = io.StringIO()
        try:
            with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
                results = cfom.run_scenarios([scenario], timeout=5, keep_going=False)
                summary = cfom.build_summary(results, total_scenarios=1)
                cfom.print_summary(summary)
                code = cfom.exit_code_for(summary)
        finally:
            cfom.REPO = old_repo
            cfom.run_crate_tests = old_run_tests
        return code, buf.getvalue()

    def test_run_created_untracked_file_is_not_silently_left_behind(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_repo(tmp)
            before = _untracked(repo)
            code, output = self._drive(repo)
            leaked = _untracked(repo) - before
            if leaked:
                self.assertNotEqual(
                    code, 0,
                    f"run left new untracked files {sorted(leaked)} and still exited 0",
                )
                for f in leaked:
                    self.assertIn(
                        f, output,
                        f"run left {f} behind without naming it in its output",
                    )

    def test_preexisting_untracked_file_is_not_deleted(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_repo(tmp)
            keep = repo / "crates/fake/notes.txt"
            keep.write_text("mine\n", encoding="utf-8")
            self._drive(repo)
            self.assertTrue(keep.exists(), "pre-existing untracked file was deleted")
            self.assertEqual(keep.read_text(encoding="utf-8"), "mine\n")


if __name__ == "__main__":
    unittest.main()
