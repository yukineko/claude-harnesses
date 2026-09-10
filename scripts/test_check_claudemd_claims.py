#!/usr/bin/env python3
"""Unit tests for scripts/check-claudemd-claims.py (dedicated CLAUDE.md claim gate).

Stdlib-only (`unittest`), no third-party dependency, no network:
    python3 scripts/test_check_claudemd_claims.py

WHY THIS GATE IS SEPARATE. check-doc-claims.py used to fold CLAUDE.md (an
instruction/config file) into its docs/**/*.md scope. This gate is the split:
CLAUDE.md's `path:line`「quote」 claims are verified here, on their own, by
reusing the SAME claim-extraction/verification engine that lives in
check-doc-claims.py (loaded via importlib, not duplicated). See that module's
docstring for the full claim-syntax / finding-kind / exemption contract; this
file only pins the properties specific to being the DEDICATED CLAUDE.md gate:

  1. F -> P, observed RED before GREEN: a CLAUDE.md whose cited quote is wrong
     blocks (exit 1); the identical fixture with the quote corrected is clean
     (exit 0). See ClaudemdFtoP below for both halves in one test, and the RED
     transcript recorded in this session's report.
  2. Missing CLAUDE.md is NOT vacuously clean. An instruction file the repo is
     supposed to have but does not is a failure to observe the norm surface,
     the same tri-state discipline check-doc-claims.py applies to an empty
     doc-set: Undetermined -> exit 2, never exit 0.
  3. A read/decode failure on CLAUDE.md is Undetermined -> exit 2 (fail-closed,
     same asymmetry as check-doc-claims.py: missing-cited-file is an ANSWER,
     unreadable-cited-file is a FAILURE TO OBSERVE).
  4. The gate has NO bypass/opt-out flag of any kind (only --repo and --json,
     which scope/format the run rather than skip verification). Grepped here
     so a future addition of `--allow`/`--skip`/`--no-verify`-style flag to the
     parser is caught by this pin rather than discovered in review.
  5. The `doc-claim-exempt` marker still works, inherited unmodified from the
     shared engine.
  6. THE JUDGED ARTIFACT IS THE GIT INDEX, inherited from the same engine and
     therefore pinned separately HERE too: a fix that lands on one of two
     mirrors is this repo's most-repeated audit finding. `--source
     {index,worktree}` with default `index`; CLAUDE.md ITSELF is read from the
     index in index mode; a CLAUDE.md that exists in the working tree but is
     not staged is Undetermined (exit 2), because a file the commit will not
     record is not an observation of the norm surface; and a non-git `--repo`
     is Undetermined rather than a working-tree fallback. See section 7 at the
     bottom of this file for the measured defect that forced the change.

Every test builds a THROWAWAY repo tree under tempfile.TemporaryDirectory() and
invokes the script as a subprocess with `--repo <tmp>`; the real repository's
CLAUDE.md is never read or written by these tests. Since (6), each throwaway
tree is a REAL `git init`ed repository and `TempTree.write()` also stages the
file, so index and working tree agree unless a test deliberately pulls them
apart. The git environment is pinned hermetic (GIT_CONFIG_GLOBAL /
GIT_CONFIG_SYSTEM = /dev/null, GIT_CONFIG_NOSYSTEM=1, a throwaway HOME) for
both the fixtures and the gate subprocess.
"""

from __future__ import annotations

import atexit
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
SCRIPT = _HERE / "check-claudemd-claims.py"
ENGINE_SCRIPT = _HERE / "check-doc-claims.py"

# A throwaway HOME for every `git` process these fixtures (and the gate itself)
# spawn. Together with the GIT_CONFIG_* trio below this makes the fixtures
# HERMETIC: the developer's global gitignore, hooks path, templatedir or
# `core.autocrlf` cannot reach into a temp repo and change what a test
# measures. A fixture that quietly picks up the ambient machine is not an
# observation.
FIXTURE_HOME = tempfile.mkdtemp(prefix="claudemd-claims-home-")
atexit.register(shutil.rmtree, FIXTURE_HOME, True)

GIT_HERMETIC = {
    "GIT_CONFIG_GLOBAL": "/dev/null",
    "GIT_CONFIG_SYSTEM": "/dev/null",
    "GIT_CONFIG_NOSYSTEM": "1",
    "GIT_TERMINAL_PROMPT": "0",
    "HOME": FIXTURE_HOME,
}

ENV = {
    "LC_ALL": "C",
    "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    "PYTHONIOENCODING": "utf-8",
    **GIT_HERMETIC,
}


def git(root, *args, input_bytes: bytes | None = None, check: bool = True):
    """Run a fixture `git` command under the hermetic env, loudly.

    `check=True` because a silently-failed `git add` would leave an EMPTY
    index, and an empty index is exactly the state some of these tests use as
    their expected result -- a quiet fixture failure would become a passing
    test that observed nothing.
    """
    proc = subprocess.run(
        ["git", "-C", str(root), *args],
        capture_output=True,
        env=ENV,
        input=input_bytes,
    )
    if check and proc.returncode != 0:
        raise AssertionError(
            "fixture git command failed (rc={}): git -C {} {}\n"
            "stdout={!r}\nstderr={!r}".format(
                proc.returncode, root, " ".join(args), proc.stdout, proc.stderr
            )
        )
    return proc


# 1 fn hello() {
# 2     // real body
# 3 }
SRC_A = """\
fn hello() {
    // real body
}
"""

PATH_A = "src/a.rs"


class TempTree:
    """A disposable repo-shaped directory that IS A REAL GIT REPOSITORY.

    MIGRATED (was: "a disposable repo-shaped directory ... no git"). This gate
    runs pre-commit and reuses check-doc-claims.py's engine, whose default
    source is now the GIT INDEX -- what the commit being made would actually
    record -- so a fixture that is only a directory has no index to judge and
    would report exit 2 for every test.

    `write()` / `write_bytes()` write the file AND stage it, so index and
    working tree agree and every pre-existing test keeps exactly the meaning
    it had. The `*_unstaged` / `stage_*` / `unstage` helpers are the only way
    the two artifacts are pulled apart, and only the tests about that
    difference use them.
    """

    def __init__(self, stack: tempfile.TemporaryDirectory):
        # .resolve() because macOS hands out /var/... symlinks to /private/...
        # and the fixture must name the repo the same way `git` does.
        self.base = Path(stack.name).resolve()
        self.root = self.base / "repo"
        self.root.mkdir(parents=True, exist_ok=True)
        # `-q` rather than `-b`: the `-b` flag only exists from git 2.28, and
        # nothing here depends on the default branch's name.
        git(self.root, "init", "-q")

    # -- staged writes: index == working tree (the pre-migration meaning) ----

    def write(self, relpath: str, content: str) -> Path:
        p = self.write_unstaged(relpath, content)
        self.stage(relpath)
        return p

    def write_bytes(self, relpath: str, raw: bytes) -> Path:
        p = self.write_bytes_unstaged(relpath, raw)
        self.stage(relpath)
        return p

    # -- working-tree-only writes (NOT staged) ------------------------------

    def write_unstaged(self, relpath: str, content: str) -> Path:
        p = self.root / relpath
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content, encoding="utf-8")
        return p

    def write_bytes_unstaged(self, relpath: str, raw: bytes) -> Path:
        p = self.root / relpath
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(raw)
        return p

    # -- index-only writes --------------------------------------------------

    def stage(self, relpath: str) -> None:
        git(self.root, "add", "--", relpath)

    def stage_content(self, relpath: str, content: str) -> Path:
        return self.stage_bytes(relpath, content.encode("utf-8"))

    def stage_bytes(self, relpath: str, raw: bytes) -> Path:
        """Put `raw` in the INDEX for relpath, leaving the WORKING TREE copy
        exactly as it was (different content, or absent)."""
        p = self.root / relpath
        p.parent.mkdir(parents=True, exist_ok=True)
        had = p.exists()
        saved = p.read_bytes() if had else None
        p.write_bytes(raw)
        self.stage(relpath)
        if had:
            p.write_bytes(saved)
        else:
            p.unlink()
        return p

    def unstage(self, relpath: str) -> None:
        """Drop the index entry, keep the working-tree file (`git rm --cached`)."""
        git(self.root, "rm", "--cached", "--quiet", "--", relpath)

    def delete_worktree_copy(self, relpath: str) -> None:
        """Remove the working-tree file, KEEP the index entry."""
        (self.root / relpath).unlink()


class GateTestCase(unittest.TestCase):
    def make_tree(self, sources: dict | None = None) -> TempTree:
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        tree = TempTree(stack)
        for rel, content in (
            {PATH_A: SRC_A} if sources is None else sources
        ).items():
            tree.write(rel, content)
        return tree

    def with_claude_md(self, body: str, sources: dict | None = None) -> TempTree:
        tree = self.make_tree(sources)
        tree.write("CLAUDE.md", body)
        return tree

    def run_gate(self, repo_path, *extra):
        # An absent implementation must not be mistaken for a genuine exit-2
        # case (python itself exits 2 on a missing script file). Fail loudly
        # HERE so every test below is a real RED, not an accidental pass.
        self.assertTrue(
            SCRIPT.exists(),
            f"implementation not present at {SCRIPT} — every test below is RED",
        )
        argv = ["python3", str(SCRIPT), "--repo", str(repo_path), *extra]
        proc = subprocess.run(argv, capture_output=True, text=True, env=ENV)
        return proc.returncode, proc.stdout, proc.stderr

    def assertBlocks(self, rc, out, err, kind=None):
        self.assertEqual(rc, 1, f"expected BLOCK (exit 1)\nSTDOUT:{out}\nSTDERR:{err}")
        if kind is not None:
            self.assertIn(kind, out + err, "the finding kind slug must be reported")

    def assertClean(self, rc, out, err):
        self.assertEqual(rc, 0, f"expected clean (exit 0)\nSTDOUT:{out}\nSTDERR:{err}")

    def assertUndetermined(self, rc, out, err):
        self.assertEqual(
            rc, 2, f"cannot-determine must exit 2, never 0\nSTDOUT:{out}\nSTDERR:{err}"
        )
        self.assertIn(
            "undetermined",
            err.lower(),
            f"exit 2 must carry a greppable `undetermined` on stderr\nSTDERR:{err}",
        )

    def json_of(self, repo_path, *extra):
        rc, out, err = self.run_gate(repo_path, "--json", *extra)
        try:
            payload = json.loads(out)
        except json.JSONDecodeError as exc:  # pragma: no cover - diagnostic path
            self.fail(f"--json stdout is not JSON ({exc})\nSTDOUT:{out}\nSTDERR:{err}")
        return rc, payload, err


# ---------------------------------------------------------------------------
# 1. F -> P: the RED before the GREEN
# ---------------------------------------------------------------------------


class ClaudemdFtoP(GateTestCase):
    def test_a_wrong_quote_blocks_then_the_same_fixture_corrected_is_clean(self):
        """RED: CLAUDE.md cites `src/a.rs:1` with a quote that is not there
        (`fn goodbye()` vs the real `fn hello()`) -> exit 1, quote-not-found.
        GREEN: the identical fixture with the quote corrected to match the
        real line -> exit 0. Both halves in one test so the F->P pairing
        cannot silently drift apart (one half changing without the other)."""
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn goodbye() {{」 for the shape.\n")
        rc, out, err = self.run_gate(tree.root)
        self.assertBlocks(rc, out, err, kind="quote-not-found")

        tree.write("CLAUDE.md", f"See `{PATH_A}:1` 「fn hello() {{」 for the shape.\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_path_not_found_blocks(self):
        tree = self.with_claude_md("See `src/gone.rs:3` for the invariant.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="path-not-found")

    def test_line_out_of_range_blocks(self):
        tree = self.with_claude_md(f"See `{PATH_A}:99` for the invariant.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="line-out-of-range")

    def test_fully_correct_claim_is_clean(self):
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn hello() {{」 for the shape.\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_doc_with_no_claims_at_all_is_clean(self):
        tree = self.with_claude_md("# Norms\n\nJust prose. Nothing cited.\n")
        self.assertClean(*self.run_gate(tree.root))


# ---------------------------------------------------------------------------
# 2. Missing CLAUDE.md is undetermined, not clean
# ---------------------------------------------------------------------------


class MissingClaudeMd(GateTestCase):
    def test_missing_claude_md_is_undetermined_not_clean(self):
        """A repo with no CLAUDE.md at all must NOT read as exit 0. An
        instruction file the repo is supposed to carry but does not is a
        failure to observe the norm surface, the same tri-state discipline
        check-doc-claims.py applies to an empty docs/ scope."""
        tree = self.make_tree()
        rc, out, err = self.run_gate(tree.root)
        self.assertUndetermined(rc, out, err)

    def test_missing_claude_md_json_verdict_agrees_with_exit_code(self):
        tree = self.make_tree()
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 2, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "undetermined")


# ---------------------------------------------------------------------------
# 3. Read/parse/repo failures are undetermined -> exit 2, fail-closed
# ---------------------------------------------------------------------------


class CannotDetermine(GateTestCase):
    def test_undecodable_claude_md_is_undetermined(self):
        tree = self.make_tree()
        tree.write_bytes("CLAUDE.md", b"# Norms\n\xff\xfe not utf-8\n")
        self.assertUndetermined(*self.run_gate(tree.root))

    def test_cited_file_that_exists_but_is_undecodable_is_undetermined(self):
        """The asymmetry that matters: a MISSING cited file is an answer about
        the claim (exit 1), a PRESENT-but-unreadable one is a failure to
        observe (exit 2)."""
        tree = self.make_tree()
        tree.write_bytes("src/binary.rs", b"fn hello() {\n\xff\xfe\n}\n")
        tree.write("CLAUDE.md", "See `src/binary.rs:1` 「fn hello() {」.\n")
        self.assertUndetermined(*self.run_gate(tree.root))

    def test_nonexistent_repo_path_is_undetermined(self):
        self.assertUndetermined(
            *self.run_gate("/nonexistent-path-for-claudemd-claims-xyz")
        )

    def test_repo_pointing_at_a_non_directory_is_undetermined(self):
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        f = Path(stack.name) / "not-a-dir.txt"
        f.write_text("i am a file\n", encoding="utf-8")
        self.assertUndetermined(*self.run_gate(f))

    def test_a_missing_engine_module_is_undetermined_not_clean(self):
        """If the sibling engine script cannot be found/loaded at all, the
        gate must not silently report clean -- it has verified nothing."""
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        fake_scripts_dir = Path(stack.name) / "scripts"
        fake_scripts_dir.mkdir()
        gate_copy = fake_scripts_dir / "check-claudemd-claims.py"
        gate_copy.write_text(SCRIPT.read_text(encoding="utf-8"), encoding="utf-8")
        # Deliberately do NOT copy check-doc-claims.py alongside it, so the
        # importlib load in _load_doc_claims_engine() fails.
        repo = Path(stack.name) / "repo"
        repo.mkdir()
        (repo / "CLAUDE.md").write_text("See `src/a.rs:1`.\n", encoding="utf-8")
        argv = ["python3", str(gate_copy), "--repo", str(repo)]
        proc = subprocess.run(argv, capture_output=True, text=True, env=ENV)
        self.assertUndetermined(proc.returncode, proc.stdout, proc.stderr)


# ---------------------------------------------------------------------------
# 4. No bypass flag of any kind
# ---------------------------------------------------------------------------


class NoBypassFlag(GateTestCase):
    def test_help_output_names_no_skip_or_allow_flag(self):
        proc = subprocess.run(
            ["python3", str(SCRIPT), "--help"],
            capture_output=True,
            text=True,
            env=ENV,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        help_text = proc.stdout.lower()
        for banned in ("--allow", "--skip", "--no-verify", "--ignore", "--force"):
            self.assertNotIn(
                banned,
                help_text,
                f"check-claudemd-claims.py must carry no bypass flag: found {banned!r}",
            )

    def test_an_unrecognized_flag_is_rejected_not_silently_accepted(self):
        """argparse's default behaviour (exit 2 on an unknown flag) is the
        correct one here and must not be papered over into a permissive
        catch-all that would let a typo'd bypass flag silently do nothing."""
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        proc = subprocess.run(
            ["python3", str(SCRIPT), "--repo", str(tree.root), "--allow-anyway"],
            capture_output=True,
            text=True,
            env=ENV,
        )
        self.assertEqual(proc.returncode, 2, proc.stderr)


# ---------------------------------------------------------------------------
# 5. The exemption marker still works (inherited from the shared engine)
# ---------------------------------------------------------------------------


class Exemption(GateTestCase):
    def test_exemption_with_a_reason_passes(self):
        tree = self.with_claude_md(
            "<!-- doc-claim-exempt: quoting a file that lives in another repo -->\n"
            "See `src/gone.rs:3` for the upstream shape.\n"
        )
        self.assertClean(*self.run_gate(tree.root))

    def test_exemption_with_no_reason_exempts_nothing(self):
        tree = self.with_claude_md(
            "<!-- doc-claim-exempt: -->\nSee `src/gone.rs:3` for the shape.\n"
        )
        self.assertBlocks(*self.run_gate(tree.root), kind="path-not-found")

    def test_exempted_finding_is_still_visible_in_json(self):
        tree = self.with_claude_md(
            "<!-- doc-claim-exempt: upstream file, deliberately not vendored -->\n"
            "See `src/gone.rs:3` for the upstream shape.\n"
        )
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "clean")
        self.assertEqual(len(payload["findings"]), 1, f"{payload}")
        self.assertIs(payload["findings"][0]["exempt"], True)


# ---------------------------------------------------------------------------
# 6. --json shape
# ---------------------------------------------------------------------------


class JsonOutput(GateTestCase):
    def test_clean_json_shape(self):
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "clean")
        self.assertEqual(payload["findings"], [])

    def test_mismatched_json_shape_reports_claude_md_as_the_doc(self):
        tree = self.with_claude_md("See `src/gone.rs:7` 「fn hello() {」.\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "mismatched")
        self.assertEqual(len(payload["findings"]), 1, f"{payload}")
        finding = payload["findings"][0]
        self.assertEqual(finding["doc"], "CLAUDE.md")
        self.assertEqual(finding["kind"], "path-not-found")

    def test_json_does_not_change_the_exit_code(self):
        tree = self.with_claude_md(f"See `{PATH_A}:1`.\n")
        rc_plain, _, _ = self.run_gate(tree.root)
        rc_json, _, _ = self.run_gate(tree.root, "--json")
        self.assertEqual((rc_plain, rc_json), (0, 0))


# ---------------------------------------------------------------------------
# 7. THE INDEX IS THE JUDGED TREE (C7: this gate reuses check-doc-claims.py's
#    engine, so it inherits the defect AND the fix verbatim).
#
#    The defect, measured on the real repo at 9a4e400c through the sibling
#    gate: 13 UNSTAGED blank lines prepended to a cited file produced five
#    `line-drifted` blocks even though the INDEX -- what the commit being made
#    would record -- was untouched and every citation there was still correct.
#    CLAUDE.md is the file this repo's agents are instructed from and it cites
#    crate sources constantly, so a peer session's unstaged edit blocks a
#    correct commit here for exactly the same reason. The two cheap ways out
#    are a bypass (this gate deliberately has none) or rewriting CLAUDE.md to
#    cite line numbers that are FALSE for the commit being made -- i.e. the
#    gate's own pressure pushes toward writing the rot it exists to detect.
# ---------------------------------------------------------------------------

PATH_LONG = "src/long.rs"

# 40 lines; the only occurrence of the needle is on line 30.
SRC_LONG = "".join(
    ('const NEEDLE: &str = "drifted marker";\n' if i == 30 else f"// line {i}\n")
    for i in range(1, 41)
)

# The same file with 13 blank lines prepended: the needle moves 30 -> 43, just
# past the engine's LINE_DRIFT_TOLERANCE of 10. The reproduction shape.
SRC_LONG_SHIFTED = ("\n" * 13) + SRC_LONG


class ClaudemdIndexIsTheJudgedTree(GateTestCase):
    def _drifted_worktree_fixture(self) -> TempTree:
        """CLAUDE.md and the cited file are both correct AS STAGED; only the
        working-tree copy of the cited file carries the 13-line shift."""
        tree = self.make_tree(sources={PATH_LONG: SRC_LONG})
        tree.write("CLAUDE.md", f"See `{PATH_LONG}:30` 「drifted marker」.\n")
        tree.write_unstaged(PATH_LONG, SRC_LONG_SHIFTED)
        return tree

    def test_an_unstaged_shift_in_a_cited_file_does_not_block_in_index_mode(self):
        """The C2a analogue for CLAUDE.md. WOULD CATCH: check-claudemd-claims.py
        forwarding no source to the shared engine (or forwarding `worktree`),
        which would leave THIS gate reading the working tree while the sibling
        gate was fixed -- the 'fix landed on one mirror only' failure this repo
        keeps re-measuring."""
        tree = self._drifted_worktree_fixture()
        self.assertClean(*self.run_gate(tree.root, "--source", "index"))

    def test_the_default_source_is_the_index(self):
        """`.githooks/pre-commit` calls this gate with no new argument, so the
        default must be `index`. WOULD CATCH: `default="worktree"` here, which
        would ship the gate still broken while every explicit-flag test passed."""
        tree = self._drifted_worktree_fixture()
        self.assertClean(*self.run_gate(tree.root))

    def test_control_the_same_fixture_still_blocks_in_worktree_mode(self):
        """CONTROL: the fixture really does contain a >10-line drift, so `clean`
        above is a statement about WHICH artifact was read. WOULD CATCH: a
        fixture whose unstaged write never landed, or a `--source worktree`
        aliased to index."""
        tree = self._drifted_worktree_fixture()
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "worktree"), kind="line-drifted"
        )

    def test_claude_md_itself_is_read_from_the_index(self):
        """The doc side of 'one source, consistently': the STAGED CLAUDE.md is
        correct, an unstaged edit to it is not. WOULD CATCH: reading CLAUDE.md
        with `open()` while reading cited files from the index -- a split-source
        fail-open that the cited-file test above cannot see."""
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        tree.write_unstaged("CLAUDE.md", f"See `{PATH_A}:99`.\n")
        self.assertClean(*self.run_gate(tree.root, "--source", "index"))

    def test_control_the_unstaged_claude_md_edit_blocks_in_worktree_mode(self):
        """CONTROL for the test above. WOULD CATCH: a fixture bug where
        `write_unstaged` accidentally staged."""
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        tree.write_unstaged("CLAUDE.md", f"See `{PATH_A}:99`.\n")
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "worktree"), kind="line-out-of-range"
        )


class ClaudemdAntiVacuityStagedStalenessStillBlocks(GateTestCase):
    """Without these, an implementation that simply returned `clean` in index
    mode would pass every test in the class above."""

    def test_a_stale_citation_staged_in_claude_md_still_blocks(self):
        """The STAGED CLAUDE.md cites line 99 of a 3-line file; the correction
        exists only as an unstaged edit. WOULD CATCH: an index mode that falls
        back to the working tree when the two differ -- 'launder the commit with
        a fix you did not stage', the exact inverse of the defect being fixed."""
        tree = self.with_claude_md(f"See `{PATH_A}:99`.\n")
        tree.write_unstaged("CLAUDE.md", f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "index"), kind="line-out-of-range"
        )

    def test_a_cited_file_fixed_only_in_the_worktree_still_blocks(self):
        """Same anti-vacuity on the cited-file side: the INDEX copy is stale.
        WOULD CATCH: the same worktree fallback, reached through the source
        rather than through CLAUDE.md."""
        tree = self.make_tree(sources={PATH_A: "fn goodbye() {\n}\n"})
        tree.write("CLAUDE.md", f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        tree.write_unstaged(PATH_A, SRC_A)
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "index"), kind="quote-not-found"
        )

    def test_control_the_unstaged_fix_does_make_worktree_mode_clean(self):
        """CONTROL: the working-tree copy really is the fixed one. WOULD CATCH:
        a fixture where the unstaged write never landed, which would make the
        test above pass for the wrong reason."""
        tree = self.make_tree(sources={PATH_A: "fn goodbye() {\n}\n"})
        tree.write("CLAUDE.md", f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        tree.write_unstaged(PATH_A, SRC_A)
        self.assertClean(*self.run_gate(tree.root, "--source", "worktree"))


class ClaudemdIndexModeUndetermined(GateTestCase):
    def test_claude_md_present_but_unstaged_is_undetermined_in_index_mode(self):
        """The existing 'a repo with no CLAUDE.md is not a clean scope' rule,
        now measured against the index: a CLAUDE.md that is not staged is not
        part of the commit, so the norm surface was not observed. WOULD CATCH:
        resolving the single explicit doc with `os.path.isfile` while reading
        its content from the index -- which would then read an absent blob and
        report a vacuous exit 0 over a file the commit does not contain."""
        tree = self.make_tree()
        tree.write_unstaged("CLAUDE.md", f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        self.assertUndetermined(*self.run_gate(tree.root, "--source", "index"))

    def test_control_the_unstaged_claude_md_is_read_in_worktree_mode(self):
        """CONTROL: the file really is there and really is correct, so exit 2
        above is about the INDEX and not about a broken fixture. WOULD CATCH: a
        fixture that never wrote CLAUDE.md at all."""
        tree = self.make_tree()
        tree.write_unstaged("CLAUDE.md", f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        self.assertClean(*self.run_gate(tree.root, "--source", "worktree"))

    def test_a_non_git_repo_is_undetermined_in_index_mode(self):
        """There is no index to judge, so there is no verdict. WOULD CATCH: an
        implementation that swallows the `git` failure and falls back to the
        working tree -- the fail-open shape this change exists to remove,
        reintroduced as a 'graceful degradation'. The fixture is a perfectly
        correct tree, so a fallback would report a confident exit 0."""
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        root = Path(stack.name).resolve() / "plain"
        (root / "src").mkdir(parents=True)
        (root / "src" / "a.rs").write_text(SRC_A, encoding="utf-8")
        (root / "CLAUDE.md").write_text(
            f"See `{PATH_A}:1` 「fn hello() {{」.\n", encoding="utf-8"
        )
        self.assertUndetermined(*self.run_gate(root, "--source", "index"))
        # ... and the same tree IS clean in worktree mode, so the exit 2 above
        # is about the missing index rather than about a malformed fixture.
        self.assertClean(*self.run_gate(root, "--source", "worktree"))

    def test_undetermined_json_has_the_verdict_and_no_findings(self):
        """WOULD CATCH: emitting `verdict: clean` (or omitting the verdict) on
        the undetermined path, which downstream reads as 'checked and fine'."""
        tree = self.make_tree()
        tree.write_unstaged("CLAUDE.md", f"See `{PATH_A}:1`.\n")
        rc, payload, err = self.json_of(tree.root, "--source", "index")
        self.assertEqual(rc, 2, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "undetermined", f"{payload}")
        self.assertEqual(payload["findings"], [], f"{payload}")


class ClaudemdSourceFlag(GateTestCase):
    def test_help_documents_the_source_flag(self):
        """C7: the same flag, spelled the same way, on both gates. WOULD CATCH:
        the flag being added to check-doc-claims.py only, leaving this gate with
        no way to ask for the old behaviour and no auditable mode name."""
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), "--help"],
            capture_output=True,
            text=True,
            env=ENV,
        )
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("--source", proc.stdout)

    def test_an_invalid_source_value_is_rejected(self):
        """WOULD CATCH: `--source` accepting arbitrary strings and silently
        treating the unknown ones as `worktree`."""
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        rc, out, err = self.run_gate(tree.root, "--source", "sideways")
        self.assertEqual(rc, 2, f"STDOUT:{out}\nSTDERR:{err}")

    def test_json_reports_index_as_the_source_used(self):
        """The mode must be auditable from the JSON alone. WOULD CATCH: the
        `source` key missing on this gate (it builds its own payload rather
        than reusing the engine's, so it can drift independently)."""
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        rc, payload, err = self.json_of(tree.root, "--source", "index")
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload.get("source"), "index", f"{payload}")

    def test_json_reports_worktree_as_the_source_used(self):
        """The other half: a hard-coded `"source": "index"` passes the test
        above and fails this one. WOULD CATCH exactly that mutation."""
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        rc, payload, err = self.json_of(tree.root, "--source", "worktree")
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload.get("source"), "worktree", f"{payload}")

    def test_json_default_run_reports_index_as_the_source(self):
        """WOULD CATCH: `default="worktree"`, which no exit-code test on an
        agreeing fixture can see."""
        tree = self.with_claude_md(f"See `{PATH_A}:1` 「fn hello() {{」.\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload.get("source"), "index", f"{payload}")

    def test_the_source_flag_is_not_a_bypass(self):
        """`--source worktree` selects an ARTIFACT; it must not become a way to
        make a real finding disappear. WOULD CATCH: implementing `worktree` as
        'skip verification' (or as an always-clean mode), which would turn the
        new flag into the escape hatch this gate deliberately does not have."""
        tree = self.with_claude_md("See `src/gone.rs:3`.\n")
        for flags in ((), ("--source", "index"), ("--source", "worktree")):
            with self.subTest(flags=flags):
                self.assertBlocks(
                    *self.run_gate(tree.root, *flags), kind="path-not-found"
                )


if __name__ == "__main__":
    unittest.main(verbosity=2)
