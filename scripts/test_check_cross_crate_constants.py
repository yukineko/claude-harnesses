#!/usr/bin/env python3
"""Unit tests for scripts/check-cross-crate-constants.py.

Stdlib-only (`unittest`), no network. Two layers:

  1. `read_value` in isolation — the primitive every PIN entry is built on
     (missing file / zero matches / multiple matches / exactly one match).
  2. End-to-end over a real throwaway git repo, with fixture files placed at the
     EXACT paths the live PINS table hard-codes
     (`crates/backlog/src/lock.rs`, `crates/condukt/src/wt_reconcile.rs`) and
     content shaped to the exact regexes in that table. The script resolves its
     root via `git rev-parse --show-toplevel`, so `git init`-ing a temp dir and
     writing files into it (no `git add`/`commit` needed — `read_value` reads
     the working tree, not the index) is sufficient to redirect it away from
     the real repository.

The important negative case is the zero-match one: a renamed constant must
BLOCK, not read as "nothing to compare, so nothing is wrong". A gate that
treated `re.findall` returning `[]` as "no drift found" would pass every other
case here and only this one would catch it.
"""
import importlib.util
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_cross_crate_constants", _HERE / "check-cross-crate-constants.py"
)
cccc = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(cccc)

REPO_ROOT = _HERE.parent

# The exact relative paths and regexes the live PINS table uses for its one
# entry. Fixture content below must satisfy these regexes to exercise the
# real, unmodified gate end-to-end.
CANON_REL = "crates/backlog/src/lock.rs"
CANON_LINE = "pub(crate) const LOCK_STALE_TTL_SECS: i64 = {value};"
COPY_REL = "crates/condukt/src/wt_reconcile.rs"
COPY_LINE = "const DRIVER_STALE_TTL_SECS: i64 = {value};"


def _git(repo, *args, check=True):
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True, text=True, check=check,
    )


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


class ReadValue(unittest.TestCase):
    """The primitive: `read_value(root, rel, pattern) -> (value, error)`, with
    exactly one of the pair set. No git involved — plain filesystem + regex."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.addCleanup(self._tmp.cleanup)

    def test_single_match_returns_value_and_no_error(self):
        _write(self.root / "f.rs", "pub(crate) const X: i64 = 42;\n")
        value, err = cccc.read_value(
            self.root, "f.rs", r"^pub\(crate\) const X: i64 = (\d+);"
        )
        self.assertEqual(value, "42")
        self.assertIsNone(err)

    def test_missing_file_is_an_error_not_a_pass(self):
        value, err = cccc.read_value(self.root, "nope.rs", r"(\d+)")
        self.assertIsNone(value)
        self.assertIsNotNone(err)
        self.assertIn("nope.rs", err)

    def test_zero_matches_is_an_error_not_a_pass(self):
        """The load-bearing case. A renamed constant must not silently read as
        'no drift' — read_value must hand back an error, never (None, None) or
        a falsy value that a careless caller could treat as agreement."""
        _write(self.root / "f.rs", "pub(crate) const X_RENAMED: i64 = 42;\n")
        value, err = cccc.read_value(
            self.root, "f.rs", r"^pub\(crate\) const X: i64 = (\d+);"
        )
        self.assertIsNone(value)
        self.assertIsNotNone(err)
        self.assertIn("renamed or restructured", err)

    def test_multiple_matches_is_an_error_not_a_guess(self):
        _write(
            self.root / "f.rs",
            "pub(crate) const X: i64 = 1;\npub(crate) const X: i64 = 2;\n",
        )
        value, err = cccc.read_value(
            self.root, "f.rs", r"^pub\(crate\) const X: i64 = (\d+);"
        )
        self.assertIsNone(value)
        self.assertIsNotNone(err)
        self.assertIn("2 declarations", err)

    def test_exactly_one_of_value_or_error_is_set(self):
        """Guards the (value, error) contract itself so a future edit cannot
        accidentally return both or neither."""
        _write(self.root / "ok.rs", "pub(crate) const X: i64 = 1;\n")
        for rel, pattern in [
            ("ok.rs", r"^pub\(crate\) const X: i64 = (\d+);"),
            ("missing.rs", r"(\d+)"),
            ("ok.rs", r"^pub\(crate\) const NOPE: i64 = (\d+);"),
        ]:
            value, err = cccc.read_value(self.root, rel, pattern)
            self.assertEqual(
                (value is None, err is None).count(True), 1,
                f"exactly one of (value, err) must be None for {rel!r}",
            )


class FixtureRepo(unittest.TestCase):
    """End-to-end over the real, unmodified check-cross-crate-constants.py,
    driven through a throwaway git repo shaped like the real PINS table."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self._tmp.name)
        _git(self.repo, "init", "-q")
        self.addCleanup(self._tmp.cleanup)

    def _seed(self, canon_text, copy_text):
        _write(self.repo / CANON_REL, canon_text)
        _write(self.repo / COPY_REL, copy_text)

    def _run(self):
        return subprocess.run(
            ["python3", str(_HERE / "check-cross-crate-constants.py")],
            cwd=str(self.repo), capture_output=True, text=True,
        )

    # a. agreement -> exit 0
    def test_agreeing_values_pass(self):
        self._seed(CANON_LINE.format(value=3600) + "\n", COPY_LINE.format(value=3600) + "\n")
        proc = self._run()
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)

    # b. copy differs from canonical -> exit 1, message names both values
    def test_differing_values_block_and_message_names_both(self):
        self._seed(CANON_LINE.format(value=3600) + "\n", COPY_LINE.format(value=1800) + "\n")
        proc = self._run()
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("1800", proc.stderr)
        self.assertIn("3600", proc.stderr)
        self.assertIn(COPY_REL, proc.stderr)
        self.assertIn(CANON_REL, proc.stderr)

    # c. canonical constant renamed -> regex matches zero times -> exit 1
    def test_canonical_renamed_blocks_not_reads_as_no_drift(self):
        """The important one. If a maintainer changes read_value's zero-match
        branch to `return "", None` or otherwise treats no-hits as agreement,
        this is the only case in the suite that catches it: the copy still has
        its original, unrelated value and nothing here would coincidentally
        collide unless the bug were exactly 'zero matches == pass'."""
        self._seed(
            "pub(crate) const LOCK_STALE_TTL_SECONDS: i64 = 3600;\n",
            COPY_LINE.format(value=3600) + "\n",
        )
        proc = self._run()
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("canonical", proc.stderr)

    # d. copy constant renamed -> regex matches zero times -> exit 1
    def test_copy_renamed_blocks(self):
        self._seed(
            CANON_LINE.format(value=3600) + "\n",
            "const DRIVER_STALE_TTL_SECONDS: i64 = 3600;\n",
        )
        proc = self._run()
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn(COPY_REL, proc.stderr)

    # e. a pattern matching more than once -> exit 1
    def test_ambiguous_canonical_blocks(self):
        self._seed(
            CANON_LINE.format(value=3600) + "\n" + CANON_LINE.format(value=7200) + "\n",
            COPY_LINE.format(value=3600) + "\n",
        )
        proc = self._run()
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("ambiguous", proc.stderr)

    def test_ambiguous_copy_blocks(self):
        self._seed(
            CANON_LINE.format(value=3600) + "\n",
            COPY_LINE.format(value=3600) + "\n" + COPY_LINE.format(value=1800) + "\n",
        )
        proc = self._run()
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("ambiguous", proc.stderr)

    # f. a cited file missing entirely -> exit 1
    def test_missing_canonical_file_blocks(self):
        _write(self.repo / COPY_REL, COPY_LINE.format(value=3600) + "\n")
        # CANON_REL deliberately never written.
        proc = self._run()
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn(CANON_REL, proc.stderr)

    def test_missing_copy_file_blocks(self):
        _write(self.repo / CANON_REL, CANON_LINE.format(value=3600) + "\n")
        # COPY_REL deliberately never written.
        proc = self._run()
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn(COPY_REL, proc.stderr)

    def test_both_cited_files_missing_still_blocks(self):
        """Adversarial: neither file is written at all — an empty repo. This is
        the one shape of 'missing file' that a value-level comparison cannot
        catch by accident. If `read_value` were ever changed to swallow an
        OSError as `("", None)` instead of an error, the single-file-missing
        cases above still go red (the present side's real value collides with
        the swallowed side's fake empty-string value and gets reported as a
        mismatch) — but with BOTH sides swallowed to the SAME fake value ("" ==
        ""), that accidental safety net vanishes and the pin reads as
        agreement. Confirmed by construction: mutating read_value's OSError
        branch to `return "", None` makes the real repo's other missing-file
        tests still fail (by accident), but this one flips to a false exit 0."""
        # Neither CANON_REL nor COPY_REL is written — repo is otherwise empty.
        proc = self._run()
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)

    # g. anti-vacuity: at least one exit-0 case exists (test_agreeing_values_pass
    # above), and a value match embedded only in a comment must not count as a
    # real declaration for either side.
    def test_value_only_in_a_comment_does_not_satisfy_the_canonical_regex(self):
        """Adversarial: the canonical regex is anchored with `^pub(crate) const
        NAME: i64 = (\\d+);`, so a value that appears only in a // comment or
        doc line must not be picked up as if it were the declaration. If it
        were, a canonical file could have its real `const` deleted entirely
        and still pass as long as a comment happened to mention a number."""
        self._seed(
            "// LOCK_STALE_TTL_SECS used to be: 3600\n"
            "pub(crate) const LOCK_STALE_TTL_SECS: i64 = 7200;\n",
            COPY_LINE.format(value=3600) + "\n",
        )
        proc = self._run()
        # The real declaration (7200) disagrees with the copy (3600); the
        # commented-out old value must not be read as the canonical value.
        self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)
        self.assertIn("7200", proc.stderr)

    def test_outside_a_git_repo_blocks(self):
        """git cannot be asked -> exit 1, never 0 (fail-closed on 'not even a
        repo', not just on a repo that disagrees)."""
        with tempfile.TemporaryDirectory() as bare:
            proc = subprocess.run(
                ["python3", str(_HERE / "check-cross-crate-constants.py")],
                cwd=bare, capture_output=True, text=True,
                env={"PATH": "/usr/bin:/bin", "GIT_CEILING_DIRECTORIES": bare},
            )
            self.assertEqual(proc.returncode, 1, proc.stdout + proc.stderr)


class RealRepoState(unittest.TestCase):
    def test_the_real_repo_currently_agrees(self):
        """The real repo's own PIN must currently pass. This is the other half
        of anti-vacuity: it proves the gate is not permanently red, and that
        the fixture-based exit-0 case above is not the only way to see green."""
        proc = subprocess.run(
            ["python3", str(_HERE / "check-cross-crate-constants.py")],
            cwd=str(REPO_ROOT), capture_output=True, text=True,
        )
        self.assertEqual(proc.returncode, 0, proc.stdout + proc.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=1)
