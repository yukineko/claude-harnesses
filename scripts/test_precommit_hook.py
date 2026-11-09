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
    "check-fail-open-diff.py",
    "check-doc-duplication.py",
    "check-plugin-versions.py",
    "check-version-bumped.py",
    "check-hardcoded-secret.py",
    "check-raw-io-ratchet.py",
    "check-fault-injection-adoption.py",
    "check-worktree-isolation.py",
    "check-gate-crates-sync.py",
    "check-gate-protection.py",
    "check-cross-crate-constants.py",
    "check-launcher-exec-bit.py",
    "check-clippy-lints.py",
    "check-backlog-store.py",
    "check-projkey-store-classification.py",
    "check-file-growth.py",
]

# Label the hook prints for each scanner, used to check the message names the
# right gate.
LABELS = {
    "check-prompt-injection.py": "injectguard",
    "check-fail-open.py": "fail-open-guard",
    "check-fail-open-diff.py": "fail-open-diff",
    "check-doc-duplication.py": "doc-duplication",
    "check-plugin-versions.py": "version-lockstep",
    "check-version-bumped.py": "bump-on-change",
    "check-hardcoded-secret.py": "secret-guard",
    "check-raw-io-ratchet.py": "raw-io-ratchet",
    "check-fault-injection-adoption.py": "fault-injection-adoption",
    "check-worktree-isolation.py": "worktree-isolation",
    "check-gate-crates-sync.py": "gate-crates-sync",
    "check-gate-protection.py": "gate-protection",
    "check-cross-crate-constants.py": "crossx-constants",
    "check-launcher-exec-bit.py": "launcher-exec-bit",
    "check-clippy-lints.py": "clippy-lints",
    "check-backlog-store.py": "backlog-store",
    "check-projkey-store-classification.py": "projkey-classification",
    "check-file-growth.py": "file-growth",
}

_STUB = """#!/usr/bin/env python3
import os, sys
with open(os.environ["HOOK_TEST_LOG"], "a") as fh:
    fh.write({name!r} + "\\n")
sys.stderr.write({name!r} + ": stub diagnostic\\n")
sys.exit({code})
"""


def _which(name):
    path = shutil.which(name)
    if path is None:  # pragma: no cover - environment defect
        raise unittest.SkipTest("required tool not on PATH: " + name)
    return path


class HookHarness:
    """A throwaway git repo containing a copy of the hook and stub scanners."""

    def __init__(self, exits=None, missing=(), with_python=True):
        """exits: {scanner_basename: exit_code}, default 0 for all present ones.
        missing: scanner basenames to NOT create at all.
        with_python: whether python3 is visible on the hook's PATH.
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
        scanner = "check-doc-duplication.py"
        h = self.harness(exits={scanner: 2})
        proc = h.run()
        self.assertBlocked(proc)
        self.assertIn("UNDETERMINED", proc.stderr)
        self.assertIn(LABELS[scanner], proc.stderr)
        # ... and specifically NOT as the generic wording used for exit 1.
        self.assertNotIn("blocked (exit 2)", proc.stderr)

    def test_exit_one_is_not_labelled_undetermined(self):
        """The distinction must go both ways, or it carries no information."""
        h = self.harness(exits={"check-doc-duplication.py": 1})
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
            exits={"check-prompt-injection.py": 1, "check-doc-duplication.py": 2}
        )
        proc = h.run()
        self.assertBlocked(proc)
        self.assertEqual(h.ran(), EXPECTED_SCANNERS)
        self.assertIn("injectguard", proc.stderr)
        self.assertIn("doc-duplication", proc.stderr)
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


class ProjkeyClassificationIsGated(HookTestCase):
    """charter DoD14(a): every consumer of harness_core::projkey must be
    classified shared or worktree-local, and an unclassified one must STOP the
    commit -- a table nobody enforces does not keep the count of unclassified
    consumers at zero (measured 2026-10-23 at a19ee218: the checker exited 1
    with 4 findings and no hook ran it).

    Uses the REAL checker, not a stub, against a throwaway tree, so the block
    is caused by the unclassified consumer itself.  The first half is the
    anti-vacuity control: the same tree with every consumer classified must be
    allowed, otherwise a hook (or a checker) that always blocks would pass.
    """

    _TABLE_ENTRY = (
        '[[consumer]]\n'
        'file = "crates/demo/src/lib.rs"\n'
        'symbol = "{symbol}"\n'
        "count = 1\n"
        'scope = "shared"\n'
        'kind = "store-key"\n'
        'evidence = "test fixture"\n'
    )

    @staticmethod
    def _stage(h, *paths):
        subprocess.run(
            [_which("git"), "-C", str(h.root), "add", "--", *map(str, paths)],
            check=True,
            capture_output=True,
        )

    def test_unclassified_projkey_consumer_blocks_the_commit(self):
        h = self.harness()
        checker = "check-projkey-store-classification.py"
        shutil.copy2(_HERE / checker, h.root / "scripts" / checker)
        # pre-commit runs it with --staged, which imports the shared scoping
        # helper next to it (user ruling 2026-11-08).
        shutil.copy2(_HERE / "staged_scope.py", h.root / "scripts" / "staged_scope.py")
        src = h.root / "crates" / "demo" / "src"
        src.mkdir(parents=True)
        table = h.root / "scripts" / "projkey-store-classification.toml"
        lib = src / "lib.rs"

        # Control: one consumer, classified -> allowed.
        lib.write_text(
            "use std::path::Path;\n"
            "use harness_core::projkey::repo_root;\n"
            "pub fn a(p: &Path) { let _ = repo_root(p); }\n"
        )
        table.write_text(self._TABLE_ENTRY.format(symbol="repo_root"))
        # The hook runs the checker with --staged, so the fixture must STAGE the
        # consumer and the table; an unstaged tree is (correctly) out of scope
        # and would make both halves of this test vacuous "skipped" runs.
        self._stage(h, lib, table)
        proc = h.run()
        self.assertNotIn("skipped", proc.stdout, "control must reach the real checker")
        self.assertEqual(
            proc.returncode,
            0,
            "a fully classified tree must be allowed\n--- stderr ---\n%s"
            % proc.stderr,
        )

        # Same tree plus one UNCLASSIFIED consumer -> blocked, and named.
        lib.write_text(
            "use std::path::Path;\n"
            "use harness_core::projkey::{project_key, repo_root};\n"
            "pub fn a(p: &Path) { let _ = repo_root(p); }\n"
            "pub fn b(p: &Path) { let _ = project_key(p); }\n"
        )
        self._stage(h, lib)
        proc = h.run()
        self.assertBlocked(proc, "when a projkey consumer is unclassified")
        self.assertIn("UNCLASSIFIED", proc.stderr)
        self.assertIn("crates/demo/src/lib.rs::project_key", proc.stderr)
        self.assertIn("projkey-classification", proc.stderr)


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
