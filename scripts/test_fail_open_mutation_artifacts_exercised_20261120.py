#!/usr/bin/env python3
"""Backlog b663c5f5, exercised for real: does a fail-open mutation run leave
test-written files behind in the invoking working tree?

Why a new test: test_fail_open_mutation_leaves_no_artifacts.py passes
vacuously. Its stub has the pre-scratch call shape `stub(crate, timeout=600)`,
check-fail-open-mutation.py calls
`run_crate_tests(crate, timeout=..., test_args=...)`, the call raises
TypeError, the scenario becomes status "error" and the stub (the part that
plays proptest and writes the artifact) never runs (backlog d5c404c0, XFAIL).

Here the stub has the REAL call shape and plays proptest faithfully: it writes
the regression seed file relative to the tree cargo runs in (cwd, defaulting
to the module's _ACTIVE_SCRATCH, exactly as run_crate_tests resolves it),
because proptest persists `proptest-regressions/` next to the crate's sources
in the tree being built.

Anti-vacuity: every test asserts the stub was actually called and actually
wrote its file. A sensitivity control proves the leak measurement fires when
a file IS written into the invoking tree, so a GREEN here is not the
measurement being blind.

DOES NOT PROVE: real cargo/proptest path resolution (the stub models it), a
test that writes through an absolute path into the invoking tree, or files
written outside both trees.
"""
import contextlib
import importlib.util
import io
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open_mutation_exercised", _HERE / "check-fail-open-mutation.py"
)
cfom = importlib.util.module_from_spec(_SPEC)
exec(  # noqa: S102
    compile(_SPEC.loader.get_source(_SPEC.name), _SPEC.origin, "exec", dont_inherit=True),
    cfom.__dict__,
)

ARTIFACT_REL = "crates/fake/proptest-regressions/verdict_monotonicity.txt"
RED_OUTPUT = "running 1 test\ntest p ... FAILED\n\ntest result: FAILED. 0 passed; 1 failed;\n"


def _run(cmd, cwd):
    return subprocess.run(
        cmd, cwd=str(cwd), stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, check=True
    )


def _make_repo(tmp):
    repo = Path(tmp).resolve()
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


class ArtifactsExercised(unittest.TestCase):
    def _drive(self, repo, write_into):
        """Run one scenario through run_scenarios with a real-call-shape stub.
        `write_into(cwd)` returns the tree root the stub writes the artifact
        under. Returns (exit_code, output, calls)."""
        scenario = cfom.Scenario(
            crate="fake", file_rel="crates/fake/src/lib.rs", description="d",
            old="    true\n", new="    false\n",
        )
        calls = []

        def stub(crate, timeout=600, test_args=None, cwd=None):
            root = Path(cwd if cwd is not None else cfom._ACTIVE_SCRATCH)
            art = Path(write_into(root)) / ARTIFACT_REL
            art.parent.mkdir(parents=True, exist_ok=True)
            art.write_text("cc deadbeef # shrinks to input = []\n", encoding="utf-8")
            calls.append({"root": root, "artifact": art, "written": art.is_file()})
            return 101, RED_OUTPUT

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
        return code, buf.getvalue(), calls

    def test_proptest_like_artifact_does_not_leak_into_invoking_tree(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_repo(tmp)
            before = _untracked(repo)
            code, output, calls = self._drive(repo, write_into=lambda root: root)

            # Anti-vacuity: the stub really ran, in a tree that is not the
            # invoking one, and really wrote the artifact there.
            self.assertEqual(len(calls), 1, f"stub not called exactly once; output:\n{output}")
            self.assertTrue(calls[0]["written"], "stub did not write its artifact")
            self.assertNotEqual(
                calls[0]["root"].resolve(), repo,
                "cargo ran in the invoking tree, not a scratch copy",
            )
            self.assertIn("caught", output, f"scenario did not reach status caught:\n{output}")

            leaked = _untracked(repo) - before
            if leaked:
                self.assertNotEqual(
                    code, 0, f"run left new untracked files {sorted(leaked)} and still exited 0"
                )
                for f in leaked:
                    self.assertIn(f, output, f"run left {f} behind without naming it")
            self.assertFalse(
                os.path.lexists(calls[0]["root"]),
                f"scratch copy {calls[0]['root']} (holding the artifact) was not removed",
            )
            self.assertEqual(code, 0, f"caught scenario with clean cleanup exited {code}:\n{output}")

    def test_control_leak_measurement_fires_when_invoking_tree_is_written(self):
        # Sensitivity control for the measurement above, not for the script:
        # a stub that writes into the invoking tree must show up as a leak.
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_repo(tmp)
            before = _untracked(repo)
            _code, output, calls = self._drive(repo, write_into=lambda _root: repo)
            self.assertEqual(len(calls), 1, f"stub not called exactly once; output:\n{output}")
            self.assertIn(ARTIFACT_REL, _untracked(repo) - before)


if __name__ == "__main__":
    unittest.main()
