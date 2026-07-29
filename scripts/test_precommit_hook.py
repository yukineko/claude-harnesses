#!/usr/bin/env python3
"""Unit tests for .githooks/pre-commit (the repository's PRIMARY gate surface).

Stdlib-only (`unittest`), no third-party dependency, so it runs identically in
CI and locally:  `python3 scripts/test_precommit_hook.py`.

The hook is exercised as a SUBPROCESS inside a throwaway git repository that has
its own `scripts/` directory full of stub scanners whose exit codes this test
chooses.  Nothing here touches the real repository, the real scanners, or the
git index of any working tree.

Load-bearing properties pinned here (the fail-closed contract stated in the
hook's own header):

  1. A clean tree exits 0.  Without this the rest proves nothing — a hook that
     blocked unconditionally would satisfy every other case below.
  2. A scanner that exits 1 (a finding) blocks the commit.
  3. A scanner that exits 2 (verdict UNDETERMINED) blocks the commit *and* is
     named as undetermined in stderr, not folded into a generic failure.  This
     is the shape a real scanner bug produced (backlog `f68ebcad`), and the one
     the previous hook was most likely to wave through.
  4. A scanner FILE that is absent blocks.  "Could not check" is not "clean".
  5. python3 missing from PATH blocks — every gate is unrunnable, so the commit
     is uncertified.
  6. Every scanner in the list actually runs.  Observed directly: each stub
     appends its own name to a log file, so the test can assert on the exact
     set and order of scanners the hook invoked — proving neither that the hook
     short-circuits after the first failure nor that a scanner is silently
     skipped.

The hook under test defaults to `.githooks/pre-commit` next to this file's repo
root; set PRECOMMIT_HOOK_UNDER_TEST to point the same suite at another copy
(used to observe the suite going RED against the previous version of the hook).
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_REPO = _HERE.parent
_HOOK = Path(
    os.environ.get("PRECOMMIT_HOOK_UNDER_TEST", _REPO / ".githooks" / "pre-commit")
).resolve()

# The scanners the hook is contractually required to run, in order.  Hardcoded
# on purpose: if the hook silently drops one, this list is what notices.
EXPECTED_SCANNERS = [
    "check-prompt-injection.py",
    "check-fail-open.py",
    "check-doc-claims.py",
    "check-claudemd-claims.py",
    "check-test-weakening.py",
    "check-plugin-versions.py",
    "check-version-bumped.py",
    "check-hardcoded-secret.py",
    "check-raw-io-ratchet.py",
    "check-worktree-isolation.py",
]

# Label the hook prints for each scanner, used to check the message names the
# right gate.
LABELS = {
    "check-prompt-injection.py": "injectguard",
    "check-fail-open.py": "fail-open-guard",
    "check-doc-claims.py": "doc-claims",
    "check-claudemd-claims.py": "claudemd-claims",
    "check-test-weakening.py": "test-weakening",
    "check-plugin-versions.py": "version-lockstep",
    "check-version-bumped.py": "bump-on-change",
    "check-hardcoded-secret.py": "secret-guard",
    "check-raw-io-ratchet.py": "raw-io-ratchet",
    "check-worktree-isolation.py": "worktree-isolation",
}

_STUB = """#!/usr/bin/env python3
import os, sys
with open(os.environ["HOOK_TEST_LOG"], "a") as fh:
    fh.write({name!r} + "\\n")
sys.stderr.write({name!r} + ": stub diagnostic\\n")
sys.exit({code})
"""

# The hook's main-tree-guard (CLAUDE.md 8) shells out to `condukt guard
# main-tree`.  It is NOT one of the EXPECTED_SCANNERS -- it runs outside the
# `run` helper -- so it gets its own stub and deliberately does not write to
# HOOK_TEST_LOG, which would corrupt the scanner-order assertion.
#
# /bin/sh rather than `#!/usr/bin/env python3`: the with_python=False cases
# withhold python3 from the hook's PATH on purpose, and a condukt stub that
# died of a missing interpreter would block for a reason the test did not mean
# to exercise, making that case pass for the wrong cause.
_CONDUKT_STUB = """#!/bin/sh
echo "condukt stub: $*" >&2
exit {code}
"""


def _which(name):
    path = shutil.which(name)
    if path is None:  # pragma: no cover - environment defect
        raise unittest.SkipTest("required tool not on PATH: " + name)
    return path


class HookHarness:
    """A throwaway git repo containing a copy of the hook and stub scanners."""

    def __init__(self, exits=None, missing=(), with_python=True, condukt_exit=0):
        """exits: {scanner_basename: exit_code}, default 0 for all present ones.
        missing: scanner basenames to NOT create at all.
        with_python: whether python3 is visible on the hook's PATH.
        condukt_exit: exit code for the stubbed `condukt guard main-tree` call.
            0 means the section-8 guard allows the commit, which is what the
            control case needs.  None provides no condukt binary at all, which
            is the gate's cannot-determine input and must block.
        """
        # .resolve() so the path matches what `git rev-parse --show-toplevel`
        # reports on macOS (/var -> /private/var).
        self.root = Path(tempfile.mkdtemp(prefix="precommit-hook-test-")).resolve()
        self.log = self.root / "ran.log"
        self.log.write_text("")

        subprocess.run(
            [_which("git"), "init", "-q", str(self.root)],
            check=True,
            capture_output=True,
        )

        scripts = self.root / "scripts"
        scripts.mkdir()
        exits = exits or {}
        for scanner in EXPECTED_SCANNERS:
            if scanner in missing:
                continue
            target = scripts / scanner
            target.write_text(_STUB.format(name=scanner, code=exits.get(scanner, 0)))
            target.chmod(0o755)

        self.hook = self.root / "pre-commit"
        shutil.copy2(_HOOK, self.hook)
        self.hook.chmod(0o755)

        # A PATH holding only the tools the hook is allowed to find.  python3 is
        # included or withheld deliberately; git and cat are always needed
        # (`git rev-parse`, and `cat >&2 <<EOF` on some /bin/sh builds).
        self.bindir = self.root / "bin"
        self.bindir.mkdir()
        tools = ["git", "cat", "sh", "env"]
        if with_python:
            tools.append("python3")
        for tool in tools:
            os.symlink(_which(tool), self.bindir / tool)

        self.env = {
            "PATH": str(self.bindir),
            "HOME": str(self.root),
            "HOOK_TEST_LOG": str(self.log),
            "LC_ALL": "C.UTF-8",
            "GIT_CONFIG_NOSYSTEM": "1",
        }

        # CONDUKT_BIN is the guard's first candidate, so pointing it at the stub
        # keeps the lookup off target/ and off PATH -- the throwaway repo has
        # neither, and the developer's real build must not leak in either.
        if condukt_exit is not None:
            condukt = self.root / "condukt-stub"
            condukt.write_text(_CONDUKT_STUB.format(code=condukt_exit))
            condukt.chmod(0o755)
            self.env["CONDUKT_BIN"] = str(condukt)

    def run(self):
        return subprocess.run(
            [str(self.hook)],
            cwd=str(self.root),
            env=self.env,
            capture_output=True,
            text=True,
        )

    def ran(self):
        return [line for line in self.log.read_text().splitlines() if line]

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


class HookTestCase(unittest.TestCase):
    def harness(self, **kwargs):
        h = HookHarness(**kwargs)
        self.addCleanup(h.cleanup)
        return h

    def assertBlocked(self, proc, msg=""):
        self.assertNotEqual(
            proc.returncode,
            0,
            "hook must BLOCK (non-zero exit) %s\n--- stdout ---\n%s\n--- stderr ---\n%s"
            % (msg, proc.stdout, proc.stderr),
        )


class CleanTreePasses(HookTestCase):
    """Property 1 — the control case.  A hook that always blocks is useless."""

    def test_all_scanners_clean_exits_zero(self):
        h = self.harness()
        proc = h.run()
        self.assertEqual(
            proc.returncode,
            0,
            "clean tree must be allowed\n--- stderr ---\n%s" % proc.stderr,
        )
        self.assertEqual(h.ran(), EXPECTED_SCANNERS)


class MainTreeGuardResolvesRestrictively(HookTestCase):
    """The section-8 guard runs outside `run`, so `run`'s contract does not
    cover it.  Its own undetermined path is asserted here.

    This was found red: the guard was added to the hook after this file was
    written, the harness never supplied a condukt, and so the control case
    above had been failing on a clean checkout of main.  The guard was right --
    it could not reach a verdict and blocked, exactly as CLAUDE.md 3 requires.
    What was missing was a test that said so on purpose rather than by
    accident.
    """

    def test_absent_condukt_binary_blocks(self):
        h = self.harness(condukt_exit=None)
        proc = h.run()
        self.assertBlocked(proc, "when no condukt binary can be found")
        self.assertIn("main-tree-guard", proc.stderr)
        # The message must say the gate could not RUN.  "could not determine"
        # and "determined you may not commit" are different facts, and a reader
        # who is told the wrong one goes looking in the wrong place.
        self.assertIn("could not run", proc.stderr)

    def test_guard_exit_two_reads_as_undetermined(self):
        h = self.harness(condukt_exit=2)
        proc = h.run()
        self.assertBlocked(proc, "when the guard exits 2")
        self.assertIn("UNDETERMINED", proc.stderr)

    def test_guard_finding_blocks(self):
        h = self.harness(condukt_exit=1)
        proc = h.run()
        self.assertBlocked(proc, "when the guard reports a finding")
        self.assertIn("main-tree-guard", proc.stderr)


class FindingBlocks(HookTestCase):
    """Property 2 — exit 1 from any scanner stops the commit."""

    def test_exit_one_blocks(self):
        for scanner in EXPECTED_SCANNERS:
            with self.subTest(scanner=scanner):
                h = self.harness(exits={scanner: 1})
                proc = h.run()
                self.assertBlocked(proc, "when %s exits 1" % scanner)
                self.assertIn(LABELS[scanner], proc.stderr)


class UndeterminedBlocksAndIsNamed(HookTestCase):
    """Property 3 — exit 2 blocks AND reads as undetermined, not as generic.

    This is the case backlog `f68ebcad` produced in the wild: a scanner that
    could not reach a verdict at all.  If the outcome or the wording collapses
    into the ordinary-finding path, the reader is told the wrong thing about
    what the gate knows.
    """

    def test_exit_two_blocks(self):
        for scanner in EXPECTED_SCANNERS:
            with self.subTest(scanner=scanner):
                h = self.harness(exits={scanner: 2})
                proc = h.run()
                self.assertBlocked(proc, "when %s exits 2" % scanner)

    def test_exit_two_stderr_says_undetermined(self):
        scanner = "check-test-weakening.py"
        h = self.harness(exits={scanner: 2})
        proc = h.run()
        self.assertBlocked(proc)
        self.assertIn("UNDETERMINED", proc.stderr)
        self.assertIn(LABELS[scanner], proc.stderr)
        # ... and specifically NOT as the generic wording used for exit 1.
        self.assertNotIn("blocked (exit 2)", proc.stderr)

    def test_exit_one_is_not_labelled_undetermined(self):
        """The distinction must go both ways, or it carries no information."""
        h = self.harness(exits={"check-test-weakening.py": 1})
        proc = h.run()
        self.assertBlocked(proc)
        self.assertNotIn("UNDETERMINED", proc.stderr)


class MissingScannerBlocks(HookTestCase):
    """Property 4 — an absent scanner file is 'cannot determine', so it blocks."""

    def test_missing_scanner_blocks(self):
        for scanner in EXPECTED_SCANNERS:
            with self.subTest(scanner=scanner):
                h = self.harness(missing=(scanner,))
                proc = h.run()
                self.assertBlocked(proc, "when %s is absent" % scanner)
                self.assertIn(scanner, proc.stderr)
                self.assertIn(LABELS[scanner], proc.stderr)

    def test_missing_scanner_does_not_stop_the_others(self):
        h = self.harness(missing=("check-prompt-injection.py",))
        proc = h.run()
        self.assertBlocked(proc)
        self.assertEqual(h.ran(), EXPECTED_SCANNERS[1:])


class MissingPythonBlocks(HookTestCase):
    """Property 5 — no python3 means no gate ran at all; that is not clean."""

    def test_no_python3_blocks(self):
        h = self.harness(with_python=False)
        proc = h.run()
        self.assertBlocked(proc, "when python3 is not on PATH")
        self.assertIn("python3", proc.stderr)
        self.assertEqual(h.ran(), [], "no scanner can have run without python3")


class EveryScannerIsRun(HookTestCase):
    """Property 6 — observed directly from the stubs' own execution log."""

    def test_all_scanners_run_in_order_on_a_clean_tree(self):
        h = self.harness()
        h.run()
        self.assertEqual(h.ran(), EXPECTED_SCANNERS)

    def test_first_failure_does_not_hide_later_scanners(self):
        h = self.harness(exits={EXPECTED_SCANNERS[0]: 1})
        proc = h.run()
        self.assertBlocked(proc)
        self.assertEqual(
            h.ran(),
            EXPECTED_SCANNERS,
            "hook must not short-circuit after the first finding — later defects "
            "would stay invisible until the author fixed the first one",
        )

    def test_multiple_failures_are_all_reported(self):
        h = self.harness(
            exits={"check-prompt-injection.py": 1, "check-test-weakening.py": 2}
        )
        proc = h.run()
        self.assertBlocked(proc)
        self.assertEqual(h.ran(), EXPECTED_SCANNERS)
        self.assertIn("injectguard", proc.stderr)
        self.assertIn("test-weakening", proc.stderr)
        self.assertIn("UNDETERMINED", proc.stderr)

    def test_hook_source_invokes_exactly_the_expected_scanner_list(self):
        """Guards the log-based tests: if the hook stopped listing a scanner,
        the stubs above would simply never be asked for, and a set comparison of
        'what ran' against 'what the hook asked for' would still agree."""
        invoked = [
            line.split()[1]
            for line in _HOOK.read_text().splitlines()
            if line.startswith("run ") and len(line.split()) >= 2
        ]
        self.assertEqual(invoked, EXPECTED_SCANNERS)


class NotAGitRepoBlocks(HookTestCase):
    """Adjacent to property 4: if the repo root cannot be resolved, the hook
    cannot even locate the scanners, which is again 'cannot determine'."""

    def test_outside_a_work_tree_blocks(self):
        h = self.harness()
        outside = Path(tempfile.mkdtemp(prefix="precommit-hook-nonrepo-")).resolve()
        self.addCleanup(shutil.rmtree, str(outside), True)
        env = dict(h.env)
        env["GIT_CEILING_DIRECTORIES"] = str(outside.parent)
        proc = subprocess.run(
            [str(h.hook)],
            cwd=str(outside),
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertBlocked(proc, "when run outside a git work tree")


if __name__ == "__main__":
    unittest.main(verbosity=2)
