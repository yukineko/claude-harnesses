#!/usr/bin/env python3
"""Unit tests for scripts/check-test-weakening.py (test-weakening gate).

Stdlib-only (`unittest`), no third-party dependency, no network:
    python3 scripts/test_check_test_weakening.py

WHY THIS GATE EXISTS (backlog 7e5e6cb8). This repo's top norm forbids silencing
a red test to make a gate pass. But when an implementation and its tests land in
the SAME commit, weakening the *test* turns the gate green and nothing detects
it — the deletion of an assertion is indistinguishable from a clean run. This
script diffs the changed test surface and BLOCKS (non-zero exit) when the test
surface got weaker.

Load-bearing properties pinned here:

  1. Each of the 4 weakening kinds (`assertion-removed`, `test-removed`,
     `ignore-added`, `should-panic-added`) is DETECTED -> exit 1.
  2. False-positive discipline, or the gate gets disabled: adding tests,
     refactoring at constant assertion count, and production (non-test) code
     losing an `assert!` are all exit 0.
  3. Surface scoping works in BOTH directions: `#[cfg(test)]` inline modules are
     in scope (not only `tests/` dir files), production code is out of scope.
  4. The acknowledgment escape hatch is EXACT: `test-weakening-justified:
     <path>:<kind> — reason` acknowledges that one finding and nothing else. A
     mismatched path, a mismatched kind, and — most important — a BARE
     `test-weakening-justified:` with no target acknowledge NOTHING. A blanket
     pass would re-create the very fail-open this gate exists to prevent.
  5. CANNOT DETERMINE IS EXIT 2, NEVER 0. Not a git repo, unresolvable base ref:
     the gate must block and say `undetermined` on stderr. A gate that resolves
     "I could not look" into "clean" is worse than no gate at all.
  6. `--json` reports {"verdict", "findings":[{path,kind,detail,acknowledged}]}
     for both a weakened and a clean run, and does NOT change the exit code.

Every test builds a THROWAWAY git repo under tempfile.TemporaryDirectory() and
invokes the script as a subprocess with `--repo <tmp> --base <ref>`; the real
repository's git state is never read or written.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
SCRIPT = _HERE / "check-test-weakening.py"

# The gate must run in a bare environment (CI containers have no git identity),
# so commits are made with explicit author/committer env rather than user config.
GIT_ENV = {
    "GIT_AUTHOR_NAME": "Test Author",
    "GIT_AUTHOR_EMAIL": "test@example.invalid",
    "GIT_COMMITTER_NAME": "Test Author",
    "GIT_COMMITTER_EMAIL": "test@example.invalid",
    "GIT_CONFIG_GLOBAL": os.devnull,
    "GIT_CONFIG_SYSTEM": os.devnull,
    "GIT_CONFIG_NOSYSTEM": "1",
    "HOME": "/nonexistent-home-for-tests",
    "LC_ALL": "C",
    "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
}

BASE_REF = "gate-base"

# ---------------------------------------------------------------------------
# Rust fixture sources
# ---------------------------------------------------------------------------

TESTS_RS_BASE = """\
use super::*;

#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
    assert!(true);
}

#[test]
fn subtracts() {
    assert_eq!(3 - 1, 2);
    assert_ne!(3 - 1, 3);
}
"""

# One `assert_ne!` deleted -> net decrease in assertion macros.
TESTS_RS_ASSERTION_REMOVED = """\
use super::*;

#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
    assert!(true);
}

#[test]
fn subtracts() {
    assert_eq!(3 - 1, 2);
}
"""

# A whole `#[test]` fn deleted -> net decrease in test attributes.
TESTS_RS_TEST_REMOVED = """\
use super::*;

#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
    assert!(true);
}
"""

TESTS_RS_IGNORE_ADDED = """\
use super::*;

#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
    assert!(true);
}

#[test]
#[ignore = "flaky on CI"]
fn subtracts() {
    assert_eq!(3 - 1, 2);
    assert_ne!(3 - 1, 3);
}
"""

TESTS_RS_SHOULD_PANIC_ADDED = """\
use super::*;

#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
    assert!(true);
}

#[test]
#[should_panic(expected = "boom")]
fn subtracts() {
    assert_eq!(3 - 1, 2);
    assert_ne!(3 - 1, 3);
}
"""

# Same assertion count and same test count, different shape: a real refactor.
TESTS_RS_REFACTORED = """\
use super::*;

fn two() -> i32 {
    2
}

#[test]
fn adds() {
    let sum = 1 + 1;
    assert_eq!(sum, two());
    assert!(sum > 0);
}

#[test]
fn subtracts() {
    let diff = 3 - 1;
    assert_eq!(diff, two());
    assert_ne!(diff, 3);
}
"""

TESTS_RS_MORE_ASSERTIONS = """\
use super::*;

#[test]
fn adds() {
    assert_eq!(1 + 1, 2);
    assert!(true);
    assert_ne!(1 + 1, 3);
}

#[test]
fn subtracts() {
    assert_eq!(3 - 1, 2);
    assert_ne!(3 - 1, 3);
    assert!(3 - 1 > 0);
}
"""

NEW_TESTS_RS = """\
#[test]
fn brand_new() {
    assert!(1 < 2);
    assert_eq!(2, 2);
}
"""

# Production code that itself uses `assert!` as a runtime invariant. Losing one
# is NOT a test weakening — the gate must not fire (surface scoping).
LIB_RS_BASE = """\
pub fn halve(n: u32) -> u32 {
    assert!(n % 2 == 0, "n must be even");
    assert_ne!(n, 0);
    n / 2
}
"""

LIB_RS_ASSERT_DROPPED = """\
pub fn halve(n: u32) -> u32 {
    assert!(n % 2 == 0, "n must be even");
    n / 2
}
"""

# Inline `#[cfg(test)]` module inside a normal source file: also in scope.
LIB_RS_WITH_CFG_TEST = """\
pub fn halve(n: u32) -> u32 {
    assert!(n % 2 == 0, "n must be even");
    n / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(halve(4), 2);
        assert_ne!(halve(4), 4);
    }
}
"""

# ...with one inline-module assertion deleted, production code untouched.
LIB_RS_CFG_TEST_WEAKENED = """\
pub fn halve(n: u32) -> u32 {
    assert!(n % 2 == 0, "n must be even");
    n / 2
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn halves() {
        assert_eq!(halve(4), 2);
    }
}
"""

TEST_PATH = "crates/demo/tests/arith.rs"
SECOND_TEST_PATH = "crates/demo/tests/second.rs"
LIB_PATH = "crates/demo/src/lib.rs"


# ---------------------------------------------------------------------------
# Throwaway git repo helpers
# ---------------------------------------------------------------------------


class TempRepo:
    """A disposable git repo with a `gate-base` ref pinned at the base commit."""

    def __init__(self, stack: tempfile.TemporaryDirectory):
        self.root = Path(stack.name)

    def git(self, *args, check=True):
        return subprocess.run(
            ["git", *args],
            cwd=str(self.root),
            env=GIT_ENV,
            capture_output=True,
            text=True,
            check=check,
        )

    def write(self, relpath: str, content: str):
        p = self.root / relpath
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content, encoding="utf-8")

    def remove(self, relpath: str):
        (self.root / relpath).unlink()

    def commit(self, message: str):
        self.git("add", "-A")
        self.git("commit", "-m", message)

    def init_base(self, files: dict):
        self.git("init", "-b", "main")
        for rel, content in files.items():
            self.write(rel, content)
        self.commit("base: initial test surface")
        # A stable, explicitly-named base ref for `--base`.
        self.git("branch", BASE_REF)


class GateTestCase(unittest.TestCase):
    """Base case: spawns temp repos and shells out to the script under test."""

    def make_repo(self, files: dict | None = None) -> TempRepo:
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        repo = TempRepo(stack)
        repo.init_base(
            files if files is not None else {TEST_PATH: TESTS_RS_BASE, LIB_PATH: LIB_RS_BASE}
        )
        return repo

    def run_gate(self, repo_path, *extra, base=BASE_REF):
        # The implementation being ABSENT must not be mistaken for one of the
        # exit-2 cases: python itself exits 2 on a missing script file. Fail
        # loudly here instead, so every behaviour test is a genuine RED.
        self.assertTrue(
            SCRIPT.exists(),
            f"implementation not present at {SCRIPT} — every behaviour test below is RED",
        )
        argv = ["python3", str(SCRIPT), "--repo", str(repo_path)]
        if base is not None:
            argv += ["--base", base]
        argv += list(extra)
        proc = subprocess.run(argv, capture_output=True, text=True, env=GIT_ENV)
        return proc.returncode, proc.stdout, proc.stderr

    def assertBlocks(self, rc, out, err, kind=None):
        self.assertEqual(rc, 1, f"expected BLOCK (exit 1)\nSTDOUT:{out}\nSTDERR:{err}")
        if kind is not None:
            self.assertIn(kind, out + err, "the finding kind slug must be reported")

    def assertClean(self, rc, out, err):
        self.assertEqual(rc, 0, f"expected clean (exit 0)\nSTDOUT:{out}\nSTDERR:{err}")

    def json_of(self, repo_path, base=BASE_REF):
        rc, out, err = self.run_gate(repo_path, "--json", base=base)
        try:
            payload = json.loads(out)
        except json.JSONDecodeError as exc:  # pragma: no cover - diagnostic path
            self.fail(f"--json stdout is not JSON ({exc})\nSTDOUT:{out}\nSTDERR:{err}")
        return rc, payload, err


# ---------------------------------------------------------------------------
# 1. Each weakening kind is detected
# ---------------------------------------------------------------------------


class DetectsWeakening(GateTestCase):
    def test_assertion_removed_blocks(self):
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_ASSERTION_REMOVED)
        repo.commit("feat: implementation + quietly drop an assert_ne!")
        self.assertBlocks(*self.run_gate(repo.root), kind="assertion-removed")

    def test_test_removed_blocks(self):
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_TEST_REMOVED)
        repo.commit("feat: implementation + delete a whole #[test]")
        self.assertBlocks(*self.run_gate(repo.root), kind="test-removed")

    def test_ignore_added_blocks(self):
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_IGNORE_ADDED)
        repo.commit("feat: implementation + #[ignore] the failing test")
        self.assertBlocks(*self.run_gate(repo.root), kind="ignore-added")

    def test_should_panic_added_blocks(self):
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_SHOULD_PANIC_ADDED)
        repo.commit("feat: implementation + #[should_panic] to absorb the failure")
        self.assertBlocks(*self.run_gate(repo.root), kind="should-panic-added")

    def test_deleting_a_whole_test_file_blocks(self):
        """The most complete weakening there is: the file simply disappears."""
        repo = self.make_repo()
        repo.remove(TEST_PATH)
        repo.commit("chore: remove the test file entirely")
        rc, out, err = self.run_gate(repo.root)
        self.assertNotEqual(rc, 0, f"deleting a test file is not clean\nSTDOUT:{out}\nSTDERR:{err}")

    def test_uncommitted_working_tree_weakening_is_seen(self):
        """The contract diffs base...HEAD PLUS uncommitted changes, so a
        weakening that has not been committed yet must still block — otherwise
        the pre-commit path is blind exactly when it matters."""
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_ASSERTION_REMOVED)  # left uncommitted
        self.assertBlocks(*self.run_gate(repo.root), kind="assertion-removed")

    def test_staged_but_uncommitted_weakening_is_seen(self):
        """`git add`-ed but not yet committed must block too.

        Untracked NEW files are out of scope by design (a file that did not
        exist can only add coverage, never weaken it), but that exclusion must
        not be implemented as "only look at committed content": the index is
        exactly where a pre-commit gate is invoked from, so a weakening staged
        for the commit under construction is the case the gate most needs to
        catch.
        """
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_ASSERTION_REMOVED)
        repo.git("add", TEST_PATH)  # staged, deliberately NOT committed
        self.assertBlocks(*self.run_gate(repo.root), kind="assertion-removed")


# ---------------------------------------------------------------------------
# 2. False-positive discipline
# ---------------------------------------------------------------------------


class NoFalsePositives(GateTestCase):
    def test_new_test_file_adding_assertions_is_clean(self):
        repo = self.make_repo()
        repo.write("crates/demo/tests/extra.rs", NEW_TESTS_RS)
        repo.commit("test: add a brand new test file")
        self.assertClean(*self.run_gate(repo.root))

    def test_adding_assertions_to_an_existing_file_is_clean(self):
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_MORE_ASSERTIONS)
        repo.commit("test: strengthen the existing assertions")
        self.assertClean(*self.run_gate(repo.root))

    def test_refactor_preserving_assertion_count_is_clean(self):
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_REFACTORED)
        repo.commit("refactor: extract a helper; assertion count unchanged")
        self.assertClean(*self.run_gate(repo.root))

    def test_production_code_losing_an_assert_is_clean(self):
        """`assert!` in production code is a runtime invariant, not a test. It is
        off the test surface, so removing one is not this gate's business."""
        repo = self.make_repo()
        repo.write(LIB_PATH, LIB_RS_ASSERT_DROPPED)
        repo.commit("refactor: drop a redundant runtime assert in production code")
        self.assertClean(*self.run_gate(repo.root))

    def test_no_changes_at_all_is_clean(self):
        repo = self.make_repo()
        self.assertClean(*self.run_gate(repo.root))

    def test_file_created_after_base_then_gutted_is_clean_KNOWN_LIMITATION(self):
        """PINS a deliberate, known limitation of the net-vs-base semantics.

        The gate compares the test surface NET, PER FILE, base vs the worktree —
        never per-commit. Consequence pinned here: a file that did not exist at
        the base can be added strong and then gutted before merge, and the gate
        stays green, because measured against the base that file is still a
        net addition.

        This is a DECISION, not an oversight, and the trade it buys is real: a
        per-commit view would flag ordinary iteration — write a test, refactor
        it, consolidate two files into one — as weakening, and a gate that fires
        on normal work is a gate that gets disabled. Net-vs-base also matches
        what actually ships: HEAD's surface is the surface that protects main.

        Know the residual risk this leaves, because it is NOT zero. For a
        brand-new feature the new tests are the ONLY thing protecting it, so
        gutting them before merge does ship an under-tested surface, and this
        gate will not say so. The compensating control is the tdd F->P oracle
        (RED must be observed before GREEN), not this scanner. If that pairing
        ever stops holding, this test is the place to come back to: change the
        assertion here first, then the implementation.
        """
        repo = self.make_repo()
        repo.write(SECOND_TEST_PATH, NEW_TESTS_RS)  # 2 assertions, did not exist at base
        repo.commit("test: add a strong new test file")
        repo.write(SECOND_TEST_PATH, "#[test]\nfn brand_new() {\n    assert!(1 < 2);\n}\n")
        repo.commit("chore: gut the test file added earlier in this same range")
        rc, out, err = self.run_gate(repo.root)
        self.assertClean(rc, out, err)


# ---------------------------------------------------------------------------
# 3. Surface scoping: #[cfg(test)] inline modules are in scope
# ---------------------------------------------------------------------------


class InlineCfgTestSurface(GateTestCase):
    def test_cfg_test_module_weakening_is_detected(self):
        """Only scanning `tests/` dirs would leave every inline unit-test module
        unguarded — the majority of this repo's tests."""
        repo = self.make_repo({LIB_PATH: LIB_RS_WITH_CFG_TEST})
        repo.write(LIB_PATH, LIB_RS_CFG_TEST_WEAKENED)
        repo.commit("feat: implementation + drop an inline-module assertion")
        self.assertBlocks(*self.run_gate(repo.root), kind="assertion-removed")

    def test_cfg_test_module_ignore_added_is_detected(self):
        repo = self.make_repo({LIB_PATH: LIB_RS_WITH_CFG_TEST})
        repo.write(LIB_PATH, LIB_RS_WITH_CFG_TEST.replace("    #[test]", "    #[test]\n    #[ignore]"))
        repo.commit("chore: #[ignore] the inline test")
        self.assertBlocks(*self.run_gate(repo.root), kind="ignore-added")

    def test_production_change_beside_an_intact_cfg_test_module_is_clean(self):
        """Touching the production half of a file that also holds a test module
        must not be read as weakening the test module."""
        repo = self.make_repo({LIB_PATH: LIB_RS_WITH_CFG_TEST})
        repo.write(
            LIB_PATH,
            LIB_RS_WITH_CFG_TEST.replace("n / 2", "let out = n / 2;\n    out"),
        )
        repo.commit("refactor: production body only")
        self.assertClean(*self.run_gate(repo.root))


# ---------------------------------------------------------------------------
# 4. Acknowledgment escape hatch — EXACT, never blanket
# ---------------------------------------------------------------------------


class Acknowledgment(GateTestCase):
    def _weaken(self, repo, message):
        repo.write(TEST_PATH, TESTS_RS_ASSERTION_REMOVED)
        repo.commit(message)

    def test_exact_justification_acknowledges(self):
        repo = self.make_repo()
        self._weaken(
            repo,
            "chore: drop an obsolete assertion\n\n"
            f"test-weakening-justified: {TEST_PATH}:assertion-removed "
            "— the invariant it checked was deleted in the same commit",
        )
        self.assertClean(*self.run_gate(repo.root))

    def test_justification_in_an_earlier_commit_of_the_range_counts(self):
        """The acknowledgment lives anywhere in base...HEAD, not only in HEAD."""
        repo = self.make_repo()
        self._weaken(
            repo,
            "chore: drop an obsolete assertion\n\n"
            f"test-weakening-justified: {TEST_PATH}:assertion-removed — obsolete invariant",
        )
        repo.write("crates/demo/README.md", "unrelated follow-up\n")
        repo.commit("docs: unrelated follow-up commit")
        self.assertClean(*self.run_gate(repo.root))

    def test_justification_for_a_different_path_does_not_acknowledge(self):
        repo = self.make_repo()
        self._weaken(
            repo,
            "chore: drop an obsolete assertion\n\n"
            "test-weakening-justified: crates/other/tests/other.rs:assertion-removed "
            "— wrong file entirely",
        )
        self.assertBlocks(*self.run_gate(repo.root), kind="assertion-removed")

    def test_justification_for_a_different_kind_does_not_acknowledge(self):
        repo = self.make_repo()
        self._weaken(
            repo,
            "chore: drop an obsolete assertion\n\n"
            f"test-weakening-justified: {TEST_PATH}:ignore-added — wrong kind",
        )
        self.assertBlocks(*self.run_gate(repo.root), kind="assertion-removed")

    def test_bare_justification_acknowledges_nothing(self):
        """THE loophole test. A `test-weakening-justified:` with no <path>:<kind>
        must never act as a blanket pass: that would hand every future author a
        one-line way to disable the gate, re-creating the exact fail-open this
        gate exists to prevent."""
        repo = self.make_repo()
        self._weaken(
            repo,
            "chore: drop an obsolete assertion\n\ntest-weakening-justified:",
        )
        self.assertBlocks(*self.run_gate(repo.root), kind="assertion-removed")

    def test_bare_justification_with_prose_only_acknowledges_nothing(self):
        repo = self.make_repo()
        self._weaken(
            repo,
            "chore: drop an obsolete assertion\n\n"
            "test-weakening-justified: this test was obsolete, trust me",
        )
        self.assertBlocks(*self.run_gate(repo.root), kind="assertion-removed")

    def test_one_justification_does_not_cover_a_second_distinct_finding(self):
        """Acknowledging ONE finding must leave the others blocking.

        Both files exist AT THE BASE and both are weakened, so both are genuine
        net-vs-base findings; only one is justified. If a single marker were
        allowed to cover the whole run, one honest deletion would launder every
        dishonest one riding along with it.
        """
        repo = self.make_repo(
            {TEST_PATH: TESTS_RS_BASE, SECOND_TEST_PATH: TESTS_RS_BASE, LIB_PATH: LIB_RS_BASE}
        )
        repo.write(TEST_PATH, TESTS_RS_ASSERTION_REMOVED)
        repo.write(SECOND_TEST_PATH, TESTS_RS_ASSERTION_REMOVED)
        repo.commit(
            "chore: two weakenings, one justified\n\n"
            f"test-weakening-justified: {TEST_PATH}:assertion-removed — obsolete invariant"
        )
        rc, payload, err = self.json_of(repo.root)
        self.assertEqual(rc, 1, f"the unacknowledged second finding must block\n{payload}\n{err}")
        self.assertEqual(payload["verdict"], "weakened")
        by_path = {f["path"]: f for f in payload["findings"]}
        self.assertEqual(
            sorted(by_path), sorted([TEST_PATH, SECOND_TEST_PATH]),
            f"exactly the two weakened files must be reported: {payload}",
        )
        self.assertEqual(len(payload["findings"]), 2, f"one finding per file: {payload}")
        self.assertEqual(by_path[TEST_PATH]["kind"], "assertion-removed")
        self.assertEqual(by_path[SECOND_TEST_PATH]["kind"], "assertion-removed")
        self.assertIs(by_path[TEST_PATH]["acknowledged"], True,
                      f"the justified finding must be marked acknowledged: {payload}")
        self.assertIs(by_path[SECOND_TEST_PATH]["acknowledged"], False,
                      f"the unjustified finding must NOT be acknowledged: {payload}")

    def test_acknowledged_finding_is_still_reported_as_a_finding(self):
        """Acknowledged != invisible: the finding stays in the report with
        acknowledged=true, so the escape hatch is auditable rather than a way to
        make the weakening disappear from view."""
        repo = self.make_repo()
        self._weaken(
            repo,
            "chore: drop an obsolete assertion\n\n"
            f"test-weakening-justified: {TEST_PATH}:assertion-removed — obsolete invariant",
        )
        rc, payload, err = self.json_of(repo.root)
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "clean")
        acked = [f for f in payload["findings"] if f.get("acknowledged")]
        self.assertTrue(acked, f"the acknowledged finding must remain visible: {payload}")
        self.assertEqual(acked[0]["kind"], "assertion-removed")


# ---------------------------------------------------------------------------
# 5. CANNOT DETERMINE -> exit 2 (never 0)
# ---------------------------------------------------------------------------


class CannotDetermineBlocks(GateTestCase):
    def _assert_undetermined(self, rc, out, err):
        self.assertEqual(
            rc, 2, f"cannot-determine must exit 2, never 0\nSTDOUT:{out}\nSTDERR:{err}"
        )
        self.assertIn(
            "undetermined",
            err.lower(),
            f"exit 2 must carry a greppable `undetermined` on stderr\nSTDERR:{err}",
        )

    def test_not_a_git_repo_is_undetermined(self):
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        (Path(stack.name) / "crates").mkdir()
        self._assert_undetermined(*self.run_gate(stack.name))

    def test_nonexistent_base_ref_is_undetermined(self):
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_ASSERTION_REMOVED)
        repo.commit("feat: weaken a test")
        self._assert_undetermined(*self.run_gate(repo.root, base="no-such-ref-xyz"))

    def test_nonexistent_repo_path_is_undetermined(self):
        self._assert_undetermined(*self.run_gate("/nonexistent-path-for-tests-xyz"))

    def test_repo_with_no_commits_is_undetermined(self):
        """An empty repo cannot resolve any base — that is "I could not look",
        not "nothing changed"."""
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        subprocess.run(
            ["git", "init", "-b", "main"],
            cwd=stack.name, env=GIT_ENV, capture_output=True, text=True, check=True,
        )
        self._assert_undetermined(*self.run_gate(stack.name))

    def test_undetermined_json_verdict_and_exit_code_agree(self):
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        rc, payload, err = self.json_of(stack.name)
        self.assertEqual(rc, 2, f"--json must not soften the exit code\n{payload}\n{err}")
        self.assertEqual(payload["verdict"], "undetermined")

    def test_unreadable_BASE_blob_is_undetermined_not_clean_KNOWN_RED(self):
        """KNOWN RED — a CONFIRMED fail-open in the implementation, left failing
        on purpose (this repo's norm: a red test is a detection, and silencing
        it is worse than the defect).

        `blob_at()` treats EVERY non-zero `git show` exit as "the file did not
        exist at the base" and returns "". Absence is only ONE of the reasons
        `git show` exits non-zero. When it fails for any other reason, the base
        side is silently counted as an empty surface, so `after < before` can
        never hold and NO weakening on that file is detectable — the run reports
        `clean`, exit 0.

        This contradicts the script's own docstring, which lists "a tracked file
        cannot be read or decoded" among the exit-2 cases. Reproduced by
        corrupting the base blob: the identical weakening that exits 1 with an
        intact object exits 0 once the base side becomes unreadable. That is the
        gate reporting a confident PASS about a range it could not read.

        Fix belongs in the implementation, not here: establish existence at the
        base from the tree (e.g. `git cat-file -e <rev>:<path>` / `ls-tree`) and
        let only a genuine ABSENCE map to "", raising Undetermined otherwise.
        """
        repo = self.make_repo()
        blob = repo.git("rev-parse", f"{BASE_REF}:{TEST_PATH}").stdout.strip()
        repo.write(TEST_PATH, TESTS_RS_ASSERTION_REMOVED)
        repo.commit("feat: weaken a test")
        # Sanity: with an intact base object this exact state blocks.
        rc, out, err = self.run_gate(repo.root)
        self.assertEqual(rc, 1, f"precondition: the weakening blocks normally\n{out}\n{err}")
        # Now make the BASE side unreadable for a reason that is NOT absence.
        obj = repo.root / ".git" / "objects" / blob[:2] / blob[2:]
        obj.chmod(0o644)
        obj.write_bytes(b"not-a-zlib-object")
        self._assert_undetermined(*self.run_gate(repo.root))

    def test_unreadable_test_file_is_undetermined_not_clean(self):
        """A file on the changed test surface that cannot be read/decoded means
        the gate does not know whether it was weakened. That is exit 2, never a
        silent 0 — a short scan reading as "clean" is this repo's signature
        fail-open."""
        repo = self.make_repo()
        # Invalid UTF-8 bytes in a file on the test surface.
        p = repo.root / TEST_PATH
        p.write_bytes(b"#[test]\nfn t() { assert!(\xff\xfe true); }\n")
        repo.commit("chore: land an undecodable test file")
        self._assert_undetermined(*self.run_gate(repo.root))


# ---------------------------------------------------------------------------
# 6. --json shape
# ---------------------------------------------------------------------------


class JsonOutput(GateTestCase):
    def test_weakened_json_shape(self):
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_ASSERTION_REMOVED)
        repo.commit("feat: implementation + quietly drop an assert_ne!")
        rc, payload, err = self.json_of(repo.root)
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "weakened")
        self.assertIsInstance(payload["findings"], list)
        self.assertTrue(payload["findings"], "a weakened verdict must carry findings")
        finding = payload["findings"][0]
        self.assertEqual(set(finding) >= {"path", "kind", "detail", "acknowledged"}, True,
                         f"finding is missing required keys: {finding}")
        self.assertEqual(finding["path"], TEST_PATH)
        self.assertEqual(finding["kind"], "assertion-removed")
        self.assertIs(finding["acknowledged"], False)
        self.assertIsInstance(finding["detail"], str)

    def test_clean_json_shape(self):
        repo = self.make_repo()
        repo.write(TEST_PATH, TESTS_RS_MORE_ASSERTIONS)
        repo.commit("test: strengthen the existing assertions")
        rc, payload, err = self.json_of(repo.root)
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "clean")
        self.assertEqual(payload["findings"], [])

    def test_json_kinds_are_the_four_stable_slugs(self):
        """The slugs are the contract's identifiers — the acknowledgment line
        keys off them, so a rename silently invalidates every justification."""
        cases = {
            TESTS_RS_ASSERTION_REMOVED: "assertion-removed",
            TESTS_RS_TEST_REMOVED: "test-removed",
            TESTS_RS_IGNORE_ADDED: "ignore-added",
            TESTS_RS_SHOULD_PANIC_ADDED: "should-panic-added",
        }
        for content, kind in cases.items():
            with self.subTest(kind=kind):
                repo = self.make_repo()
                repo.write(TEST_PATH, content)
                repo.commit(f"chore: produce a {kind} finding")
                rc, payload, err = self.json_of(repo.root)
                self.assertEqual(rc, 1, f"{payload}\n{err}")
                self.assertIn(kind, [f["kind"] for f in payload["findings"]],
                              f"expected kind {kind}: {payload}")


# ---------------------------------------------------------------------------
# 7. Residual fail-opens found by the round-26 adversarial audit
#    Each of the three "weakening blocks" tests below is EXIT 0 (fail-open) on
#    the pre-fix scanner and EXIT 1 once fixed: the F->P oracle for this round.
#    The "benign is clean" tests pin that the fix does not start over-blocking.
# ---------------------------------------------------------------------------

_BIG = "#[test]\nfn big() {\n" + "".join(
    f"    assert_eq!({i}, {i});\n" for i in range(12)
) + "}\n"
_BIG_WEAKER = "#[test]\nfn big() {\n" + "".join(
    f"    assert_eq!({i}, {i});\n" for i in range(11)
) + "}\n"

# A path whose bytes git quotes by default (core.quotePath=true) -> the quoted
# form ends in `.rs"`, so an `endswith('.rs')` filter drops it from the scan.
NONASCII_TEST_PATH = "crates/demo/tests/テスト.rs"

# A #[cfg(test)] module whose body has a `}` inside a string literal, then a
# further assertion. A raw-brace matcher closes the module at the in-string `}`
# and never counts the trailing assertion (so dropping it looks like no change).
LIB_BRACE_IN_STRING_BASE = (
    "pub fn f() -> i32 { 1 }\n\n"
    "#[cfg(test)]\nmod tests {\n"
    "    #[test]\n    fn t() {\n"
    '        let s = "}";\n        assert_eq!(s, "}");\n'
    "        assert!(true);\n"
    "    }\n}\n"
)
LIB_BRACE_IN_STRING_WEAK = (
    "pub fn f() -> i32 { 1 }\n\n"
    "#[cfg(test)]\nmod tests {\n"
    "    #[test]\n    fn t() {\n"
    '        let s = "}";\n        assert_eq!(s, "}");\n'
    "    }\n}\n"
)


class ResidualFailOpensRound26(GateTestCase):
    # --- F1: non-ASCII (git-quoted) paths ---------------------------------
    def test_nonascii_path_weakening_blocks(self):
        repo = self.make_repo({NONASCII_TEST_PATH: _BIG})
        repo.write(NONASCII_TEST_PATH, _BIG_WEAKER)
        repo.commit("weaken a test whose path is non-ASCII")
        rc, out, err = self.run_gate(repo.root)
        self.assertBlocks(rc, out, err, kind="assertion-removed")

    def test_nonascii_path_benign_is_clean(self):
        repo = self.make_repo({NONASCII_TEST_PATH: _BIG})
        repo.write(NONASCII_TEST_PATH, _BIG.replace("fn big()", "fn renamed_fn()"))
        repo.commit("rename the fn, keep every assertion")
        rc, out, err = self.run_gate(repo.root)
        self.assertClean(rc, out, err)

    # --- F2: a rename must not launder a weakening ------------------------
    def test_rename_plus_weakening_blocks(self):
        repo = self.make_repo({TEST_PATH: _BIG})
        repo.remove(TEST_PATH)
        repo.write(SECOND_TEST_PATH, _BIG_WEAKER)  # >50% similar -> git sees a rename
        repo.commit("rename the test file and drop an assertion")
        # sanity: git really detected a rename here, else the test proves nothing
        ns = repo.git("diff", "--name-status", "-M", BASE_REF, "HEAD").stdout
        self.assertTrue(ns.startswith("R"), f"expected a detected rename, got: {ns!r}")
        rc, out, err = self.run_gate(repo.root)
        self.assertBlocks(rc, out, err, kind="assertion-removed")

    def test_pure_rename_is_clean(self):
        repo = self.make_repo({TEST_PATH: _BIG})
        repo.remove(TEST_PATH)
        repo.write(SECOND_TEST_PATH, _BIG)  # identical content -> pure rename
        repo.commit("rename the test file, change nothing else")
        rc, out, err = self.run_gate(repo.root)
        self.assertClean(rc, out, err)

    def test_low_similarity_rename_still_blocks_via_deletion(self):
        # <50% similar: git reports delete+add, not a rename; the deletion of the
        # old (asserting) file must still block.
        repo = self.make_repo({TEST_PATH: _BIG})
        repo.remove(TEST_PATH)
        repo.write(SECOND_TEST_PATH, "#[test]\nfn tiny() {\n    assert_eq!(0, 0);\n}\n")
        repo.commit("replace the test file with a much smaller one")
        rc, out, err = self.run_gate(repo.root)
        self.assertBlocks(rc, out, err)

    # --- F3: braces inside string/char literals must not truncate ---------
    def test_brace_in_string_literal_weakening_blocks(self):
        repo = self.make_repo({LIB_PATH: LIB_BRACE_IN_STRING_BASE})
        repo.write(LIB_PATH, LIB_BRACE_IN_STRING_WEAK)
        repo.commit("drop an assertion sitting past a string-literal brace")
        rc, out, err = self.run_gate(repo.root)
        self.assertBlocks(rc, out, err, kind="assertion-removed")

    def test_brace_in_string_literal_benign_is_clean(self):
        repo = self.make_repo({LIB_PATH: LIB_BRACE_IN_STRING_BASE})
        repo.write(LIB_PATH, LIB_BRACE_IN_STRING_BASE.replace("fn t()", "fn renamed()"))
        repo.commit("rename the test fn, keep the assertions")
        rc, out, err = self.run_gate(repo.root)
        self.assertClean(rc, out, err)

    def test_KNOWN_LIMITATION_assert_true_not_caught(self):
        # A macro-COUNTING scanner cannot see `assert!(cond)` -> `assert!(true)`:
        # the count is unchanged. Pinned as a KNOWN LIMITATION so a reviewer does
        # not assume semantic neutering is covered here (the tdd F->P oracle is).
        repo = self.make_repo(
            {TEST_PATH: "#[test]\nfn t() {\n    assert!(1 + 1 == 2);\n}\n"}
        )
        repo.write(TEST_PATH, "#[test]\nfn t() {\n    assert!(true);\n}\n")
        repo.commit("neuter an assertion without changing the count")
        rc, out, err = self.run_gate(repo.root)
        self.assertClean(rc, out, err)  # documents the gap, not an endorsement


if __name__ == "__main__":
    unittest.main(verbosity=2)
