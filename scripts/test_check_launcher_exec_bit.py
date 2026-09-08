#!/usr/bin/env python3
"""Unit tests for scripts/check-launcher-exec-bit.py.

Stdlib-only (`unittest`), no network. Two layers:

  1. Pure functions (`launchers`, `problems`) against synthetic index entries —
     scope and verdict, with no git involved.
  2. Real throwaway git repos with `core.fileMode=false` SET, because that config
     is the whole reason this gate reads the index instead of the filesystem. A
     test that ran without it would pass for the wrong reason and would not
     notice if someone "simplified" the implementation to `os.access(X_OK)`.

The real repo's own launchers are checked too: all of them must be 100755. That
assertion is the one that would have caught the defect this gate was built for
(`crates/taintguard/bin/taintguard` staged 100644, backlog 8cb3bc22).
"""
import importlib.util
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_launcher_exec_bit", _HERE / "check-launcher-exec-bit.py"
)
cleb = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(cleb)

REPO_ROOT = _HERE.parent


def _git(repo, *args, check=True):
    return subprocess.run(
        ["git", "-C", str(repo), *args],
        capture_output=True, text=True, check=check,
    )


class Scope(unittest.TestCase):
    """What counts as a launcher. Every exclusion here is a real path in-tree."""

    def test_crate_bin_launcher_is_in_scope(self):
        self.assertEqual(
            cleb.launchers([("100755", "crates/ctxrot/bin/ctxrot")]),
            [("100755", "crates/ctxrot/bin/ctxrot")],
        )

    def test_src_bin_rust_source_is_out_of_scope(self):
        """crates/context-governor/src/bin/context-governor.rs is a Rust bin
        TARGET, not a launcher, and it is tracked 100644 legitimately. A naive
        `crates/*/bin/*` git pathspec DOES match it, because git's `*` crosses
        `/` — that is precisely why the scope is a single-level regex here."""
        self.assertEqual(
            cleb.launchers(
                [("100644", "crates/context-governor/src/bin/context-governor.rs")]
            ),
            [],
        )

    def test_dotted_basename_is_out_of_scope(self):
        """A doc or config that happens to sit in bin/ is not an executable, and
        excluding it by shape avoids an allowlist that would rot."""
        self.assertEqual(cleb.launchers([("100644", "crates/x/bin/README.md")]), [])

    def test_non_crates_path_is_out_of_scope(self):
        self.assertEqual(cleb.launchers([("100644", "scripts/bin/tool")]), [])

    def test_platform_suffixed_binary_would_be_in_scope(self):
        """Not tracked today, but if one is ever committed it must be executable
        for the same reason — so it is deliberately NOT excluded."""
        self.assertEqual(
            cleb.launchers([("100755", "crates/ctxrot/bin/ctxrot-linux-x64")]),
            [("100755", "crates/ctxrot/bin/ctxrot-linux-x64")],
        )

    def test_result_is_sorted(self):
        found = cleb.launchers(
            [("100755", "crates/b/bin/b"), ("100755", "crates/a/bin/a")]
        )
        self.assertEqual([p for _m, p in found], ["crates/a/bin/a", "crates/b/bin/b"])


class Verdict(unittest.TestCase):
    def test_all_executable_is_no_problem(self):
        self.assertEqual(cleb.problems([("100755", "crates/a/bin/a")]), [])

    def test_non_executable_is_a_problem(self):
        found = cleb.problems([("100644", "crates/taintguard/bin/taintguard")])
        self.assertEqual(len(found), 1)
        self.assertIn("crates/taintguard/bin/taintguard", found[0])
        self.assertIn("100644", found[0])
        self.assertIn("100755", found[0])

    def test_message_names_the_mode_in_words(self):
        """A bare octal number does not tell a reader what is wrong."""
        found = cleb.problems([("100644", "crates/a/bin/a")])
        self.assertIn("NOT executable", found[0])

    def test_symlink_is_a_problem_too(self):
        found = cleb.problems([("120000", "crates/a/bin/a")])
        self.assertEqual(len(found), 1)
        self.assertIn("symlink", found[0])

    def test_unrecognized_mode_is_still_a_problem(self):
        """Fail-closed on a mode this gate has never seen: an unknown mode is not
        evidence of executability."""
        found = cleb.problems([("100000", "crates/a/bin/a")])
        self.assertEqual(len(found), 1)
        self.assertIn("unrecognized mode", found[0])

    def test_only_the_offenders_are_reported(self):
        found = cleb.problems(
            [("100755", "crates/a/bin/a"), ("100644", "crates/b/bin/b")]
        )
        self.assertEqual(len(found), 1)
        self.assertIn("crates/b/bin/b", found[0])


class FixtureRepo(unittest.TestCase):
    """End-to-end over a real git index, with core.fileMode=false set."""

    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.repo = Path(self._tmp.name)
        _git(self.repo, "init", "-q")
        # THE point of this fixture. With fileMode=false, git ignores the working
        # tree's mode entirely, so nothing but the index knows the answer.
        _git(self.repo, "config", "core.fileMode", "false")
        self.addCleanup(self._tmp.cleanup)

    def _add_launcher(self, crate, name=None, executable=True):
        name = name or crate
        path = self.repo / "crates" / crate / "bin" / name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("#!/bin/sh\nexec true\n", encoding="utf-8")
        rel = f"crates/{crate}/bin/{name}"
        _git(self.repo, "add", "--", rel)
        _git(self.repo, "update-index", "--chmod=+x" if executable else "--chmod=-x", "--", rel)
        return rel

    def _run(self):
        proc = subprocess.run(
            ["python3", str(_HERE / "check-launcher-exec-bit.py")],
            cwd=str(self.repo), capture_output=True, text=True,
        )
        return proc

    def test_index_mode_is_what_is_read_not_the_filesystem(self):
        """The load-bearing assertion. The file is chmod 0777 on disk and 100644
        in the index; the gate must go RED. An implementation using
        os.access/st_mode would pass here — that is the mistake being pinned."""
        rel = self._add_launcher("taintguard", executable=False)
        os.chmod(self.repo / rel, 0o777)
        self.assertTrue(os.access(self.repo / rel, os.X_OK), "fixture precondition")
        proc = self._run()
        self.assertEqual(proc.returncode, cleb.RC_NOT_EXECUTABLE, proc.stderr)
        self.assertIn(rel, proc.stderr)

    def test_executable_launcher_passes_even_when_not_executable_on_disk(self):
        """The mirror image: 100755 in the index, 0644 on disk. Still green,
        because a fresh clone gets the index's mode. Together with the test above
        this pins that the filesystem is not consulted in EITHER direction."""
        rel = self._add_launcher("ctxrot", executable=True)
        os.chmod(self.repo / rel, 0o644)
        proc = self._run()
        self.assertEqual(proc.returncode, cleb.RC_OK, proc.stderr)

    def test_one_bad_launcher_among_many_is_found(self):
        """The measured shape of the real defect: 1 deviation in 39."""
        for i in range(5):
            self._add_launcher(f"good{i}", executable=True)
        rel = self._add_launcher("taintguard", executable=False)
        proc = self._run()
        self.assertEqual(proc.returncode, cleb.RC_NOT_EXECUTABLE, proc.stderr)
        self.assertIn("1 of 6 launcher(s) checked", proc.stderr)
        self.assertIn(rel, proc.stderr)

    def test_remedy_names_update_index_not_chmod(self):
        """`chmod +x` is a no-op under core.fileMode=false, so a remedy that says
        it would send the reader in a circle."""
        self._add_launcher("taintguard", executable=False)
        proc = self._run()
        self.assertIn("git update-index --chmod=+x", proc.stderr)
        self.assertIn("core.fileMode=false", proc.stderr)

    def test_empty_scope_is_undetermined_not_a_pass(self):
        """A repo with no launcher at all: the scope broke, or this is not the
        repo the gate thinks it is. Either way it is not a clean bill of health."""
        (self.repo / "README.md").write_text("x\n", encoding="utf-8")
        _git(self.repo, "add", "README.md")
        proc = self._run()
        self.assertEqual(proc.returncode, cleb.RC_UNDETERMINED, proc.stderr)
        self.assertIn("ZERO", proc.stderr)

    def test_src_bin_rust_file_does_not_satisfy_the_scope(self):
        """A repo whose only crates/*/bin/* match is a src/bin Rust source must
        report an EMPTY scope (undetermined), not a green over one .rs file."""
        path = self.repo / "crates" / "cg" / "src" / "bin" / "cg.rs"
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text("fn main() {}\n", encoding="utf-8")
        _git(self.repo, "add", "--", "crates/cg/src/bin/cg.rs")
        proc = self._run()
        self.assertEqual(proc.returncode, cleb.RC_UNDETERMINED, proc.stderr)

    def test_outside_a_git_repo_is_undetermined(self):
        """git cannot be asked -> exit 2, never 0."""
        with tempfile.TemporaryDirectory() as bare:
            proc = subprocess.run(
                ["python3", str(_HERE / "check-launcher-exec-bit.py")],
                cwd=bare, capture_output=True, text=True,
                env={**os.environ, "GIT_CEILING_DIRECTORIES": bare},
            )
            self.assertEqual(proc.returncode, cleb.RC_UNDETERMINED, proc.stdout + proc.stderr)


class Undetermined(unittest.TestCase):
    def test_unparseable_ls_files_record_raises_rather_than_dropping_it(self):
        """Skipping a record the parser does not understand would silently shrink
        the scope — the exact failure mode this gate exists to close — so it is
        raised instead."""
        with self.assertRaises(cleb.Undetermined):
            cleb.index_entries(repo="/nonexistent-path-for-this-test")

    def test_git_failure_message_is_carried_to_the_reader(self):
        try:
            cleb.index_entries(repo="/nonexistent-path-for-this-test")
        except cleb.Undetermined as exc:
            self.assertTrue(str(exc), "the reason must not be empty")


class RealRepoState(unittest.TestCase):
    def test_every_launcher_in_this_repo_is_executable(self):
        """The assertion that would have caught backlog 8cb3bc22 by machine."""
        found = cleb.launchers(cleb.index_entries(repo=str(REPO_ROOT)))
        self.assertTrue(found, "no launcher found — the scope is broken")
        self.assertEqual(cleb.problems(found), [])

    def test_the_scope_finds_every_plugin_that_ships_a_binary(self):
        """A count assertion, so the scope cannot quietly shrink to a subset and
        keep reporting green over it. 37 = 40 plugins minus the THREE that are
        skills-only (daily-report, scout, flow) and ship no binary.

        Was `39` with a docstring reading "41 plugins minus the two ... (three
        names)" — self-contradictory, and 41 - 3 is 38, not 39. That constant
        was already RED at HEAD before taintguard's removal (measured
        2026-08-24: 38 launchers in the index vs the 39 asserted here), because
        it was never re-derived when `flow` became the third skills-only plugin.
        Both errors are corrected together: the population is now 40 plugins
        after taintguard was retired, and 40 - 3 = 37."""
        found = cleb.launchers(cleb.index_entries(repo=str(REPO_ROOT)))
        self.assertEqual(len(found), 37, [p for _m, p in found])


if __name__ == "__main__":
    unittest.main(verbosity=1)
