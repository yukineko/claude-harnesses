#!/usr/bin/env python3
"""Unit tests for scripts/check-doc-claims.py (doc `path:line` claim gate).

Stdlib-only (`unittest`), no third-party dependency, no network:
    python3 scripts/test_check_doc_claims.py

WHY THIS GATE EXISTS (backlog c1d4e18f). This repo's CLAUDE.md was found to
carry 5 false statements, all written hours earlier. Three were MECHANICALLY
detectable: a norm described as "removed" that is still live at the cited
`path:line`, a type described as existing that `grep` finds zero of, and a
defect written in the past tense that is still present. A rotten record is as
harmful as a hidden mistake — the next implementer reasons from a false premise
and never learns the premise was false. So: extract every backtick-quoted
`<path>:<line>` claim (plus the verbatim quote attached to it) from the docs and
check it against the real files.

Load-bearing properties pinned here:

  1. Each of the 5 finding kinds (`path-not-found`, `path-escapes-repo`,
     `line-out-of-range`, `quote-not-found`, `line-drifted`) is DETECTED ->
     exit 1.
  2. False-positive discipline, or the gate gets disabled: a fully correct
     claim, a claim with no quote, and a `path:line`-shaped token that is NOT
     in backticks (ordinary prose) are all exit 0.
  3. Quote matching is whitespace-NORMALIZED but case-SENSITIVE, and all three
     delimiters (「」, "", ``) are recognized — with ONE asymmetry, measured
     rather than assumed: in markdown prose backticks mark IDENTIFIERS far more
     often than quotations (against the real repo the unconditional rule
     produced 3 false positives out of 6 findings, the "quotes" being
     `checks_verdict`, `run_ignored_test`, and another `path:line`). So a
     backticked span counts as a quote ONLY IF IT CONTAINS WHITESPACE. 「」 and
     "" stay unconditional: they are how an author explicitly opts a bare
     identifier in. Both halves of that rule are pinned here, because if they
     drift apart the escape route from a false positive disappears.
  4. Doc-set scoping works in every direction: `docs/**/*.md` is walked
     RECURSIVELY (not only the top level), `--doc` restricts the set, and a
     set that comes out EMPTY is exit 2 rather than a vacuous exit 0 -- a
     scope that has silently shrunk to nothing reports clean most convincingly
     at the moment it stopped checking anything. CLAUDE.md is deliberately NOT
     in this gate's default scope -- its claims are verified by the dedicated
     scripts/check-claudemd-claims.py gate (scripts/test_check_claudemd_claims.py),
     which reuses this same engine. Keeping CLAUDE.md folded into a generic
     docs/-citation gate blurred an instruction/config file into documentation
     verification, which is the split this repo now makes explicit.
  5. The exemption escape hatch is EXACT: `<!-- doc-claim-exempt: <reason> -->`
     on the line IMMEDIATELY BEFORE the claim. A reasonless
     `<!-- doc-claim-exempt: -->` exempts NOTHING, and a comment two lines up
     exempts nothing. A blanket pass would re-create the very rot this gate
     exists to detect. Exempted findings stay VISIBLE in `--json`.
  6. CANNOT DETERMINE IS EXIT 2, NEVER 0, and says `undetermined` on stderr.
     Note the asymmetry this pins: a cited file that is MISSING is
     `path-not-found` (exit 1 — a real answer about the claim), while a cited
     file that EXISTS AND CANNOT BE DECODED is undetermined (exit 2).
     Conflating them would let an unreadable tree read as a clean one.
  7. `--json` reports {"verdict", "findings":[{doc,doc_line,path,cited_line,
     kind,detail,exempt}]} and does NOT change the exit code.
  8. THE JUDGED ARTIFACT IS THE GIT INDEX, not the working tree. This is a
     PRE-COMMIT gate, so the tree it must report on is the one the commit
     would record. `--source {index,worktree}` selects it and DEFAULTS to
     `index`; in either mode the document and the cited file come from the
     SAME artifact (reading one from the index and the other from disk is a
     new fail-open of its own, so both directions are pinned). Untracked
     cited files stay `path-not-found` (exit 1) -- `git ls-files` answers that
     definitively, so it is an ANSWER, not a failure to observe -- while every
     way the index CANNOT be read (no repo, a subdirectory, git missing, git
     non-zero, a non-UTF-8 blob, a symlink/gitlink entry, an unmerged entry)
     is Undetermined -> exit 2. See section 8 at the bottom of this file for
     the measured defect that forced the change.

Every test builds a THROWAWAY doc tree under tempfile.TemporaryDirectory() and
invokes the script as a subprocess with `--repo <tmp>`; the real repository's
docs and sources are never read or written by these tests. Since (8), each
throwaway tree is a REAL `git init`ed repository and `TempTree.write()` also
stages the file, so index and working tree agree unless a test deliberately
pulls them apart. The git environment is pinned hermetic (GIT_CONFIG_GLOBAL /
GIT_CONFIG_SYSTEM = /dev/null, GIT_CONFIG_NOSYSTEM=1, a throwaway HOME) for
both the fixtures and the gate subprocess, so nothing about the developer's
machine can change what a test measures.
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
SCRIPT = _HERE / "check-doc-claims.py"

# A throwaway HOME for every `git` process these fixtures (and the gate itself)
# spawn. Together with the GIT_CONFIG_* trio below this makes the fixtures
# HERMETIC: the developer's global gitignore, hooks path, templatedir, or
# `core.autocrlf` cannot reach into a temp repo and change what a test measures.
# A fixture that quietly picks up the ambient machine is not an observation.
FIXTURE_HOME = tempfile.mkdtemp(prefix="doc-claims-home-")
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

    `check=True` by default and NOT optional in spirit: a fixture whose
    `git add` silently failed would leave an empty index, and an empty index
    is exactly the state several of these tests use as their EXPECTED result.
    A quiet fixture failure would therefore turn into a passing test that
    observed nothing — the vacuity this suite exists to prevent.
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

# ---------------------------------------------------------------------------
# Fixture sources (the "real files" a doc makes claims about)
# ---------------------------------------------------------------------------

# 1 // header
# 2 // second line
# 3 fn hello() {
# 4     // swallow it and exit 0
# 5 }
SRC_A = """\
// header
// second line
fn hello() {
    // swallow it and exit 0
}
"""

# Line 2 carries irregular internal whitespace, so a doc that quotes it with
# tidy single spaces must still match (and vice versa).
SRC_SPACED = """\
fn spaced() {
    let x =   1;\tlet y = 2;
}
"""

# 40 lines; the only occurrence of the needle is on line 30.
SRC_LONG = "".join(
    ("const NEEDLE: &str = \"drifted marker\";\n" if i == 30 else f"// line {i}\n")
    for i in range(1, 41)
)

PATH_A = "src/a.rs"
PATH_SPACED = "src/spaced.rs"
PATH_LONG = "src/long.rs"

DEFAULT_SOURCES = {PATH_A: SRC_A, PATH_SPACED: SRC_SPACED, PATH_LONG: SRC_LONG}


# ---------------------------------------------------------------------------
# Throwaway tree helpers
# ---------------------------------------------------------------------------


class TempTree:
    """A disposable repo-shaped directory that IS A REAL GIT REPOSITORY.

    MIGRATED (was: "a disposable repo-shaped directory ... no git"). The gate
    is a PRE-COMMIT gate and its default source is now the GIT INDEX — what
    the commit being made would actually record — so a fixture that is only a
    directory has no index to judge and would report exit 2 for every test.

    The migration deliberately keeps every pre-existing test's MEANING:
    `write()` / `write_bytes()` write the file AND stage it, so index and
    working tree agree and a test written before this change measures exactly
    what it measured before. The `*_unstaged` / `stage_*` / `unstage` helpers
    below are the ONLY way the two artifacts are pulled apart, and they are
    used only by the tests that are specifically about that difference.

    Layout: the TemporaryDirectory holds `repo/` (the git repo handed to
    `--repo`) as a subdirectory, so a test can create a sibling OUTSIDE the
    repository (the `path-escapes-repo` fixture) without polluting it.
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

    def add_symlink(self, relpath: str, target: str) -> None:
        """Stage a SYMLINK (index mode 120000), not a regular file blob."""
        p = self.root / relpath
        p.parent.mkdir(parents=True, exist_ok=True)
        os.symlink(target, p)
        self.stage(relpath)

    def add_gitlink(self, reldir: str) -> None:
        """Stage a GITLINK / submodule entry (index mode 160000).

        Built without touching the outer repo's history: a nested repo with
        one commit, then `git add <dir>` in the parent, which records a
        gitlink rather than a blob.
        """
        sub = self.root / reldir
        sub.mkdir(parents=True, exist_ok=True)
        git(sub, "init", "-q")
        (sub / "f.txt").write_text("nested\n", encoding="utf-8")
        git(sub, "add", "--", "f.txt")
        git(
            sub,
            "-c",
            "user.name=fixture",
            "-c",
            "user.email=fixture@example.invalid",
            "commit",
            "-q",
            "-m",
            "nested",
        )
        git(self.root, "add", "--", reldir)

    def make_unmerged(self, relpath: str, base: str, ours: str, theirs: str) -> None:
        """Leave `relpath` UNMERGED in the index: stages 1/2/3, no stage 0.

        Written straight into the index with `hash-object -w` +
        `update-index --index-info` rather than by performing a real merge --
        no commit, no branch, no `git merge`, and therefore deterministic.
        """
        shas = []
        for body in (base, ours, theirs):
            proc = git(
                self.root,
                "hash-object",
                "-w",
                "--stdin",
                input_bytes=body.encode("utf-8"),
            )
            shas.append(proc.stdout.decode("ascii").strip())
        # Remove any stage-0 entry first; --index-info then installs 1/2/3.
        git(self.root, "update-index", "--force-remove", "--", relpath, check=False)
        info = "".join(
            "100644 {} {}\t{}\n".format(sha, stage, relpath)
            for stage, sha in zip((1, 2, 3), shas)
        )
        git(
            self.root,
            "update-index",
            "--index-info",
            input_bytes=info.encode("utf-8"),
        )
        unmerged = git(self.root, "ls-files", "-u", "--", relpath).stdout.decode()
        if unmerged.count("\n") != 3:
            raise AssertionError(
                "fixture failed to create an unmerged index entry for "
                f"{relpath}: `git ls-files -u` says {unmerged!r}"
            )


class GateTestCase(unittest.TestCase):
    """Base case: spawns temp trees and shells out to the script under test."""

    def make_tree(self, sources: dict | None = None) -> TempTree:
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        tree = TempTree(stack)
        for rel, content in (DEFAULT_SOURCES if sources is None else sources).items():
            tree.write(rel, content)
        return tree

    def with_claude_md(self, body: str, sources: dict | None = None) -> TempTree:
        """Write `body` into CLAUDE.md. Reserved for the small number of tests
        that specifically pin CLAUDE.md's exclusion from this gate's default
        scope, or that pass CLAUDE.md explicitly via `--doc`. CLAUDE.md
        coverage of the shared engine itself lives in
        scripts/test_check_claudemd_claims.py; use `with_doc` below for
        engine-behaviour tests here."""
        tree = self.make_tree(sources)
        tree.write("CLAUDE.md", body)
        return tree

    def with_doc(
        self, body: str, sources: dict | None = None, relpath: str = "docs/test.md"
    ) -> TempTree:
        """Write `body` into a docs/**/*.md file (in this gate's default
        scope) and return the tree. This is the generic fixture for testing
        the claim-verification engine itself -- CLAUDE.md is no longer in
        check-doc-claims.py's default scope, so engine tests must not rely on
        CLAUDE.md being scanned."""
        tree = self.make_tree(sources)
        tree.write(relpath, body)
        return tree

    def run_gate(self, repo_path, *extra):
        # The implementation being ABSENT must not be mistaken for one of the
        # exit-2 cases: python itself exits 2 on a missing script file. Fail
        # loudly HERE instead, so every behaviour test below is a genuine RED
        # rather than an accidental pass.
        self.assertTrue(
            SCRIPT.exists(),
            f"implementation not present at {SCRIPT} — every behaviour test below is RED",
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
# 1. Each of the four kinds is detected
# ---------------------------------------------------------------------------


class DetectsMismatch(GateTestCase):
    def test_path_not_found_blocks(self):
        """The cited file does not exist. This is a real ANSWER about the claim
        (exit 1), not an inability to look (exit 2) — see the asymmetry test."""
        tree = self.with_doc("See `src/gone.rs:3` for the invariant.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="path-not-found")

    def test_line_out_of_range_blocks(self):
        """`src/a.rs` has 5 lines; a claim about line 99 cannot be true."""
        tree = self.with_doc(f"See `{PATH_A}:99` for the invariant.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="line-out-of-range")

    def test_quote_not_found_blocks(self):
        """The quote is nowhere in the cited file: the doc is describing code
        that does not exist. This is the 'described as removed but still live'
        class that motivated the gate, in its simplest form."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn goodbye() {{」 for the shape.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="quote-not-found")

    def test_line_drifted_blocks_and_reports_the_real_line(self):
        """The quote is still there, but the line number rotted. Reporting WHERE
        it actually is turns the finding into a one-line fix instead of a hunt."""
        tree = self.with_doc(f"See `{PATH_LONG}:5` 「drifted marker」 today.\n")
        rc, out, err = self.run_gate(tree.root)
        self.assertBlocks(rc, out, err, kind="line-drifted")
        self.assertIn(
            "30",
            out + err,
            "line-drifted must report where the quote ACTUALLY is\n"
            f"STDOUT:{out}\nSTDERR:{err}",
        )

    def test_drift_boundary_ten_lines_away_is_clean(self):
        """`within +/-10 lines` is read as INCLUSIVE: cited 20, actual 30."""
        tree = self.with_doc(f"See `{PATH_LONG}:20` 「drifted marker」.\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_drift_boundary_eleven_lines_away_blocks(self):
        """Cited 19, actual 30: 11 lines away, outside the tolerance."""
        tree = self.with_doc(f"See `{PATH_LONG}:19` 「drifted marker」.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="line-drifted")

    def test_two_bad_claims_produce_two_findings(self):
        tree = self.with_doc(
            f"First `src/gone.rs:1`.\n\nSecond `{PATH_A}:99`.\n"
        )
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(
            sorted(f["kind"] for f in payload["findings"]),
            ["line-out-of-range", "path-not-found"],
            f"both claims must be reported independently: {payload}",
        )


# ---------------------------------------------------------------------------
# 2. False-positive discipline
# ---------------------------------------------------------------------------


class NoFalsePositives(GateTestCase):
    def test_fully_correct_claim_is_clean(self):
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」 for the shape.\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_claim_without_a_quote_is_clean(self):
        """path + line only: nothing to compare beyond existence and range."""
        tree = self.with_doc(f"The barrier lives at `{PATH_A}:4`.\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_path_line_token_not_in_backticks_is_not_a_claim(self):
        """Ordinary prose must not become a claim, or every sentence mentioning
        a line number becomes a merge blocker and the gate gets switched off."""
        tree = self.with_doc(
            "see line 12 of foo.rs for context, and also src/gone.rs:99 in passing.\n"
        )
        self.assertClean(*self.run_gate(tree.root))

    def test_backticked_non_claim_tokens_are_ignored(self):
        """Backticks are used all over these docs for commands and type names.
        Only a `<path>:<line>` shape is a claim."""
        tree = self.with_doc(
            "Run `cargo test -p harness-core`, and note `Result`/`Option` and "
            "`docs/GLOSSARY.md` and `foo:bar`.\n"
        )
        self.assertClean(*self.run_gate(tree.root))

    def test_doc_with_no_claims_at_all_is_clean(self):
        tree = self.with_doc("# Title\n\nJust prose. Nothing cited.\n")
        self.assertClean(*self.run_gate(tree.root))

    # NOTE: the "no docs present is vacuously clean" case used to live here and
    # was OVERTURNED, not deleted -- see
    # DocSetScoping.test_an_empty_document_set_is_undetermined_not_clean, which
    # asserts the opposite verdict on the same input.
    #
    # "Vacuously clean" is sound logic about claims and the wrong verdict for a
    # gate. Exit 0 is consumed downstream as "the documents were checked and
    # they hold"; on an empty scope nothing was checked, so exit 0 states
    # something never observed. It is also the failure mode with no symptom:
    # rename the documents, or point the gate at the wrong root, and it reports
    # success forever. A doc set that CANNOT BE READ and a doc set that IS NOT
    # THERE are the same thing from the verdict's point of view -- neither is
    # an observation that the claims hold -- so both resolve to exit 2.


# ---------------------------------------------------------------------------
# 3. Quote matching semantics
# ---------------------------------------------------------------------------


class QuoteMatching(GateTestCase):
    def test_whitespace_is_normalized_doc_tidier_than_source(self):
        """Source has `let x =   1;\\tlet y = 2;`; the doc quotes it with single
        spaces. A byte-exact matcher would report a false quote-not-found and
        train authors to distrust the gate."""
        tree = self.with_doc(
            f"See `{PATH_SPACED}:2` 「let x = 1; let y = 2;」 for the shape.\n"
        )
        self.assertClean(*self.run_gate(tree.root))

    def test_whitespace_is_normalized_doc_looser_than_source(self):
        """The reverse direction: the doc pads the quote, the source is tidy."""
        tree = self.with_doc(
            f"See `{PATH_A}:3` 「fn   hello()    {{」 for the shape.\n"
        )
        self.assertClean(*self.run_gate(tree.root))

    def test_quote_matching_is_case_sensitive(self):
        """Case carries meaning in code (`Undetermined` vs `undetermined`), so a
        case-folded match would silently bless a wrong quote."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「FN HELLO() {{」.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="quote-not-found")

    def test_corner_bracket_delimiter_is_recognized(self):
        tree = self.with_doc(f"`{PATH_A}:3` 「fn goodbye() {{」\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="quote-not-found")

    def test_double_quote_delimiter_is_recognized(self):
        tree = self.with_doc(f'`{PATH_A}:3` "fn goodbye() {{"\n')
        self.assertBlocks(*self.run_gate(tree.root), kind="quote-not-found")

    def test_backtick_delimiter_is_recognized_when_the_span_has_whitespace(self):
        tree = self.with_doc(f"`{PATH_A}:3` `fn goodbye() {{`\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="quote-not-found")

    def test_backticked_bare_identifier_is_not_a_quote(self):
        """THE false-positive case that forced the amended rule. `goodbye` does
        not occur in the cited file, but a backticked span with no whitespace is
        an identifier reference, not a quotation — so this must be exit 0.

        Measured, not argued: run unconditionally against this repo's real docs,
        3 of 6 findings were exactly this shape. A gate that cries wolf on
        ordinary markdown gets switched off, and a switched-off gate detects
        nothing at all.
        """
        tree = self.with_doc(f"See `{PATH_A}:3`, handled by `goodbye`.\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_backticked_following_path_line_is_not_a_quote(self):
        """The other measured false positive: the next backticked span is
        another `path:line` reference. It has no whitespace, so it is not a
        quote — and it is itself a claim, checked on its own terms."""
        tree = self.with_doc(f"See `{PATH_A}:3` and `{PATH_A}:4`.\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_bare_identifier_in_corner_brackets_is_checked(self):
        """The explicit-opt-in half of the amended rule. 「」 is how an author
        says 'I really do mean this bare token as a verbatim quote'. If this
        half rots, the backtick relaxation becomes an unconditional hole."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「goodbye」.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="quote-not-found")

    def test_bare_identifier_in_double_quotes_is_checked(self):
        tree = self.with_doc(f'See `{PATH_A}:3` "goodbye".\n')
        self.assertBlocks(*self.run_gate(tree.root), kind="quote-not-found")

    def test_bare_identifier_in_corner_brackets_matches_when_present(self):
        tree = self.with_doc(f"See `{PATH_A}:3` 「hello」.\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_a_skipped_backtick_span_does_not_consume_a_later_real_quote(self):
        """A backticked identifier is NOT a quote, so scanning must continue to
        the next delimited span rather than stopping there. Otherwise an
        identifier written before the real quotation would silently disable the
        check on that line — a fail-open dressed up as false-positive
        discipline."""
        tree = self.with_doc(f"See `{PATH_A}:3` in `goodbye` 「fn goodbye() {{」.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="quote-not-found")

    def test_corner_bracket_delimiter_matches_when_correct(self):
        tree = self.with_doc(f"`{PATH_A}:3` 「fn hello() {{」\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_double_quote_delimiter_matches_when_correct(self):
        tree = self.with_doc(f'`{PATH_A}:3` "fn hello() {{"\n')
        self.assertClean(*self.run_gate(tree.root))

    def test_backtick_delimiter_matches_when_correct(self):
        tree = self.with_doc(f"`{PATH_A}:3` `fn hello() {{`\n")
        self.assertClean(*self.run_gate(tree.root))

    def test_quote_on_a_later_doc_line_is_not_attached(self):
        """`ON THE SAME LINE` is the contract. A quote on the NEXT doc line
        belongs to no claim, so this is a path+line-only claim -> clean."""
        tree = self.with_doc(f"See `{PATH_A}:3` for the shape,\n「fn goodbye() {{」\n")
        self.assertClean(*self.run_gate(tree.root))


# ---------------------------------------------------------------------------
# 4. Doc-set scoping
# ---------------------------------------------------------------------------


class DocSetScoping(GateTestCase):
    def test_docs_markdown_is_checked(self):
        tree = self.make_tree()
        tree.write("docs/stop-gate-latency.md", "The gate is at `src/gone.rs:41`.\n")
        rc, out, err = self.run_gate(tree.root)
        self.assertBlocks(rc, out, err, kind="path-not-found")
        self.assertIn("docs/stop-gate-latency.md", out + err)

    def test_claude_md_is_not_in_the_default_scope_but_docs_is(self):
        """CHANGED (was: test_claude_md_and_docs_are_both_in_the_default_set,
        which asserted the opposite verdict). CLAUDE.md moved out of this
        gate's default scope into the dedicated check-claudemd-claims.py gate
        (scripts/test_check_claudemd_claims.py pins CLAUDE.md coverage now).
        A false claim planted ONLY in CLAUDE.md here must NOT be reported by
        this gate, while a false claim in docs/ still is."""
        tree = self.with_claude_md("Root doc cites `src/gone.rs:1`.\n")
        tree.write("docs/x.md", f"Docs cites `{PATH_A}:99`.\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(
            sorted({f["doc"] for f in payload["findings"]}),
            ["docs/x.md"],
            f"CLAUDE.md must NOT be scanned by default any more: {payload}",
        )

    def test_doc_flag_restricts_the_set(self):
        tree = self.with_claude_md(f"Good claim `{PATH_A}:3`.\n")
        tree.write("docs/bad.md", "Bad claim `src/gone.rs:1`.\n")
        # Whole default set -> the bad doc blocks.
        self.assertBlocks(*self.run_gate(tree.root), kind="path-not-found")
        # Restricted to the good doc -> clean.
        self.assertClean(*self.run_gate(tree.root, "--doc", "CLAUDE.md"))

    def test_doc_flag_is_repeatable(self):
        tree = self.with_claude_md(f"Good claim `{PATH_A}:3`.\n")
        tree.write("docs/bad.md", "Bad claim `src/gone.rs:1`.\n")
        tree.write("docs/alsobad.md", "Bad claim `src/gone2.rs:1`.\n")
        rc, payload, err = self.json_of(
            tree.root, "--doc", "CLAUDE.md", "--doc", "docs/bad.md"
        )
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(
            sorted({f["doc"] for f in payload["findings"]}),
            ["docs/bad.md"],
            f"only the named docs may be scanned: {payload}",
        )

    def test_nested_docs_subdirectory_is_in_the_default_set(self):
        """The widening the previous pin asked for, made deliberately here.

        This test previously pinned the FLAT `docs/*.md` and asserted that a
        nested document was out of scope. It was rewritten -- not weakened --
        after the flat glob was fault-injected: with `docs/sub/deep.md` as the
        only document, the gate reported `{"verdict": "clean"}` exit 0 over a
        citation to a file that does not exist. Moving a document one directory
        down removed it from coverage and produced no signal of any kind, which
        is the failure this whole gate exists to catch.
        """
        tree = self.make_tree()
        tree.write("docs/sub/deep.md", "Nested cites `src/gone.rs:1`.\n")
        self.assertBlocks(*self.run_gate(tree.root), kind="path-not-found")

    def test_a_claim_escaping_the_repository_is_blocked(self):
        """`../`-style claims are not checkable and must not read as clean.

        Measured before the fix: a doc citing `../outside/secret.txt:2` with a
        quote that really did occur there returned exit 0. Two things are wrong
        with that. The verdict depends on what happens to sit beside the repo,
        so it differs between a CI runner and a laptop; and doc text arrives
        with the diff, which makes an unbounded read a capability the document
        itself gets to aim.
        """
        tree = self.make_tree()
        root = Path(tree.root)
        # A real sibling of the repo, so the claim can reach it with `../`.
        outside = Path(tempfile.mkdtemp(prefix="outside_", dir=root.parent))
        self.addCleanup(shutil.rmtree, outside, True)
        (outside / "secret.txt").write_text("line one\nTOKEN=hunter2\n")
        tree.write(
            "docs/d.md",
            f'See `../{outside.name}/secret.txt:2` which says "TOKEN=hunter2".\n',
        )
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(payload["findings"][0]["kind"], "path-escapes-repo")

    def test_a_form_feed_does_not_inflate_the_line_count(self):
        """The overcount introduced while fixing the trailing-newline overcount.

        `splitlines()` breaks on FORM FEED (and VT, NEL, U+2028/9); git, rustc
        and every editor here count only newlines. Measured before the fix: a
        2-line file holding one `\\x0c` reported 3 lines and accepted `:3`.
        """
        tree = self.make_tree()
        tree.write("src/ff.rs", "fn a() {}\n// page\x0cbreak\n")
        tree.write("docs/d.md", "The code at `src/ff.rs:3` does the thing.\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(payload["findings"][0]["kind"], "line-out-of-range")
        self.assertIn("file has 2 lines", payload["findings"][0]["detail"])

    def test_crlf_line_endings_do_not_shift_the_line_count(self):
        """CRLF must count as one line break, not one break plus a stray char."""
        tree = self.make_tree()
        tree.write("src/crlf.rs", "fn a() {}\r\nfn b() {}\r\n")
        tree.write("docs/d.md", "The code at `src/crlf.rs:3` does the thing.\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertIn("file has 2 lines", payload["findings"][0]["detail"])

    def test_an_empty_document_set_is_undetermined_not_clean(self):
        """No CLAUDE.md and no docs/ at all -> exit 2, never exit 0.

        Exit 0 would assert "every cited claim matches the tree" on the
        strength of having read no documents. It would also stay green forever
        if the documents were renamed or moved out from under the gate, which
        is the quietest way for a gate to stop working.
        """
        tree = self.make_tree()
        rc, out, err = self.run_gate(tree.root)
        self.assertEqual(rc, 2, f"empty scope must be undetermined: {out}\n{err}")
        self.assertIn("empty scope", (out + err).lower())

    def test_a_line_one_past_the_end_of_file_is_out_of_range(self):
        """The trailing-newline off-by-one, pinned.

        A file ending in `\\n` split on `\\n` yields a trailing empty element,
        so a naive length count admits a citation to EOF+1. Measured before the
        fix: a 2-line file accepted `:3`, and the message for `:4` read "file
        has 3 lines". Off-by-one on the permissive side is a fail-open.
        """
        tree = self.make_tree()
        tree.write("src/two.rs", "fn a() {}\nfn b() {}\n")
        tree.write("docs/d.md", "The code at `src/two.rs:3` does the thing.\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(payload["findings"][0]["kind"], "line-out-of-range")
        self.assertIn(
            "file has 2 lines",
            payload["findings"][0]["detail"],
            f"the reported length must be the real one: {payload}",
        )


# ---------------------------------------------------------------------------
# 5. The exemption escape hatch — EXACT, never blanket
# ---------------------------------------------------------------------------


class Exemption(GateTestCase):
    def test_exemption_with_a_reason_passes(self):
        tree = self.with_doc(
            "<!-- doc-claim-exempt: quoting a file that lives in another repo -->\n"
            "See `src/gone.rs:3` for the upstream shape.\n"
        )
        self.assertClean(*self.run_gate(tree.root))

    def test_exempted_finding_is_still_visible_in_json(self):
        """Exempt != invisible. The finding stays in the report with
        exempt=true, so the escape hatch is auditable rather than a way to make
        the stale claim disappear from view."""
        tree = self.with_doc(
            "<!-- doc-claim-exempt: upstream file, deliberately not vendored -->\n"
            "See `src/gone.rs:3` for the upstream shape.\n"
        )
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "clean")
        self.assertEqual(len(payload["findings"]), 1, f"finding must remain visible: {payload}")
        finding = payload["findings"][0]
        self.assertEqual(finding["kind"], "path-not-found")
        self.assertIs(finding["exempt"], True, f"must be marked exempt: {payload}")

    def test_exemption_with_no_reason_exempts_nothing(self):
        """THE loophole test. A reasonless `<!-- doc-claim-exempt: -->` would be
        a one-line way to switch the gate off for any claim, re-creating exactly
        the rot this gate exists to detect."""
        tree = self.with_doc(
            "<!-- doc-claim-exempt: -->\nSee `src/gone.rs:3` for the shape.\n"
        )
        self.assertBlocks(*self.run_gate(tree.root), kind="path-not-found")

    def test_exemption_with_whitespace_only_reason_exempts_nothing(self):
        tree = self.with_doc(
            "<!-- doc-claim-exempt:    -->\nSee `src/gone.rs:3` for the shape.\n"
        )
        self.assertBlocks(*self.run_gate(tree.root), kind="path-not-found")

    def test_exemption_two_lines_above_does_not_exempt(self):
        """IMMEDIATELY BEFORE means immediately. Anything looser lets one
        exemption drift down a document and quietly cover claims it was never
        written for."""
        tree = self.with_doc(
            "<!-- doc-claim-exempt: a real reason -->\n"
            "\n"
            "See `src/gone.rs:3` for the shape.\n"
        )
        self.assertBlocks(*self.run_gate(tree.root), kind="path-not-found")

    def test_exemption_does_not_carry_to_the_following_claim(self):
        """It exempts the NEXT line only, not the rest of the document."""
        tree = self.with_doc(
            "<!-- doc-claim-exempt: a real reason -->\n"
            "Exempted `src/gone.rs:3`.\n"
            "Not exempted `src/gone2.rs:3`.\n"
        )
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 1, f"the second claim must still block\n{payload}\n{err}")
        by_path = {f["path"]: f for f in payload["findings"]}
        self.assertEqual(
            sorted(by_path), ["src/gone.rs", "src/gone2.rs"], f"{payload}"
        )
        self.assertIs(by_path["src/gone.rs"]["exempt"], True, f"{payload}")
        self.assertIs(by_path["src/gone2.rs"]["exempt"], False, f"{payload}")

    def test_exemption_covers_every_claim_on_the_next_line(self):
        """Contract: it exempts the LINE, so every claim on that line."""
        tree = self.with_doc(
            "<!-- doc-claim-exempt: both files live upstream -->\n"
            "Both `src/gone.rs:3` and `src/gone2.rs:4` are upstream.\n"
        )
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(len(payload["findings"]), 2, f"both must be reported: {payload}")
        self.assertTrue(all(f["exempt"] for f in payload["findings"]), f"{payload}")


# ---------------------------------------------------------------------------
# 6. CANNOT DETERMINE -> exit 2 (never 0)
# ---------------------------------------------------------------------------


class CannotDetermine(GateTestCase):
    def test_repo_pointing_at_a_non_directory_is_undetermined(self):
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        f = Path(stack.name) / "not-a-dir.txt"
        f.write_text("i am a file\n", encoding="utf-8")
        self.assertUndetermined(*self.run_gate(f))

    def test_nonexistent_repo_path_is_undetermined(self):
        self.assertUndetermined(*self.run_gate("/nonexistent-path-for-doc-claims-xyz"))

    # NOTE: test_undecodable_doc_is_undetermined (which wrote undecodable bytes
    # to CLAUDE.md) was REMOVED here, not weakened in place. Now that CLAUDE.md
    # is out of this gate's default scope, that write would no longer be read
    # via the "undecodable doc" path at all -- with no docs/ present the scope
    # would come out EMPTY, so the test would still assert exit 2 but for the
    # WRONG reason (empty-scope Undetermined, not undecodable-doc Undetermined),
    # making it a false positive as a regression pin. It is now a duplicate of
    # test_undecodable_docs_markdown_is_undetermined immediately below, which
    # already covers "an undecodable docs/**/*.md file is undetermined" via a
    # real docs/ file. CLAUDE.md's own undecodable-file coverage moved to
    # scripts/test_check_claudemd_claims.py.
    def test_undecodable_docs_markdown_is_undetermined(self):
        tree = self.make_tree()
        tree.write_bytes("docs/broken.md", b"\xff\xfe\n")
        self.assertUndetermined(*self.run_gate(tree.root))

    def test_cited_file_that_exists_but_is_undecodable_is_undetermined(self):
        """The asymmetry that matters: a MISSING cited file is an answer about
        the claim (exit 1), a PRESENT-but-unreadable one is a failure to
        observe (exit 2)."""
        tree = self.make_tree()
        tree.write_bytes("src/binary.rs", b"fn hello() {\n\xff\xfe\n}\n")
        tree.write("docs/d.md", "See `src/binary.rs:1` 「fn hello() {」.\n")
        self.assertUndetermined(*self.run_gate(tree.root))

    def test_undecodable_cited_file_without_a_quote_is_still_undetermined(self):
        """Even a quote-less claim needs the file's LINE COUNT, so an unreadable
        file leaves the range check undetermined too — it must not degrade into
        'existence checked, good enough'."""
        tree = self.make_tree()
        tree.write_bytes("src/binary.rs", b"fn hello() {\n\xff\xfe\n}\n")
        tree.write("docs/d.md", "See `src/binary.rs:2`.\n")
        self.assertUndetermined(*self.run_gate(tree.root))

    def test_missing_vs_unreadable_cited_file_are_not_conflated(self):
        """Both cases in one test, so a future 'simplification' that collapses
        them has to delete an explicit comparison rather than quietly relax a
        single assertion."""
        tree = self.make_tree()
        tree.write("docs/d.md", "See `src/gone.rs:1`.\n")
        rc_missing, out_m, err_m = self.run_gate(tree.root)
        self.assertEqual(
            rc_missing, 1, f"missing cited file is a FINDING\n{out_m}\n{err_m}"
        )
        tree.write_bytes("src/binary.rs", b"\xff\xfe\n")
        tree.write("docs/d.md", "See `src/binary.rs:1`.\n")
        self.assertUndetermined(*self.run_gate(tree.root))

    def test_explicit_doc_that_does_not_exist_is_undetermined(self):
        """`--doc missing.md` is 'a doc in the doc set cannot be read'. Skipping
        it would let a typo in a CI invocation silently scan nothing."""
        tree = self.with_doc(f"Good claim `{PATH_A}:3`.\n")
        self.assertUndetermined(*self.run_gate(tree.root, "--doc", "docs/nope.md"))

    def test_undetermined_json_verdict_and_exit_code_agree(self):
        tree = self.make_tree()
        tree.write_bytes("docs/broken2.md", b"\xff\xfe\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 2, f"--json must not soften the exit code\n{payload}\n{err}")
        self.assertEqual(payload["verdict"], "undetermined")


# ---------------------------------------------------------------------------
# 7. --json shape
# ---------------------------------------------------------------------------


class JsonOutput(GateTestCase):
    def test_clean_json_shape(self):
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "clean")
        self.assertEqual(payload["findings"], [])

    def test_mismatched_json_shape(self):
        tree = self.with_doc(
            "# Title\n\nSee `src/gone.rs:7` 「fn hello() {」 for the shape.\n"
        )
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "mismatched")
        self.assertEqual(len(payload["findings"]), 1, f"{payload}")
        finding = payload["findings"][0]
        self.assertTrue(
            set(finding)
            >= {"doc", "doc_line", "path", "cited_line", "kind", "detail", "exempt"},
            f"finding is missing required keys: {finding}",
        )
        self.assertEqual(finding["doc"], "docs/test.md")
        self.assertEqual(finding["doc_line"], 3, "doc_line is 1-based")
        self.assertEqual(finding["path"], "src/gone.rs")
        self.assertEqual(finding["cited_line"], 7)
        self.assertEqual(finding["kind"], "path-not-found")
        self.assertIs(finding["exempt"], False)
        self.assertIsInstance(finding["detail"], str)

    def test_json_kinds_are_the_four_stable_slugs(self):
        """The slugs are the contract's identifiers, reported to humans and
        greppable in CI logs; a rename is a breaking change."""
        cases = [
            ("path-not-found", "See `src/gone.rs:3`.\n"),
            ("line-out-of-range", f"See `{PATH_A}:99`.\n"),
            ("quote-not-found", f"See `{PATH_A}:3` 「fn goodbye() {{」.\n"),
            ("line-drifted", f"See `{PATH_LONG}:5` 「drifted marker」.\n"),
        ]
        for kind, body in cases:
            with self.subTest(kind=kind):
                tree = self.with_doc(body)
                rc, payload, err = self.json_of(tree.root)
                self.assertEqual(rc, 1, f"{payload}\n{err}")
                self.assertEqual(
                    [f["kind"] for f in payload["findings"]],
                    [kind],
                    f"expected exactly {kind}: {payload}",
                )

    def test_json_does_not_change_the_exit_code_for_a_clean_run(self):
        tree = self.with_doc(f"See `{PATH_A}:3`.\n")
        rc_plain, _, _ = self.run_gate(tree.root)
        rc_json, _, _ = self.run_gate(tree.root, "--json")
        self.assertEqual((rc_plain, rc_json), (0, 0))


# ---------------------------------------------------------------------------
# 8. THE INDEX IS THE JUDGED TREE (backlog: the pre-commit gate read the wrong
#    artifact). Everything below this line is new with `--source {index,
#    worktree}`, default `index`.
#
#    Measured defect being pinned: on the real repo at 9a4e400c, prepending 13
#    unstaged blank lines to crates/condukt/skills/condukt/SKILL.md made
#    `check-doc-claims.py --doc docs/plugin-dependency-graph.md` emit five
#    `[BLOCK] ... line-drifted` findings and exit 1, while the INDEX — exactly
#    what a commit of the other paths would record — was untouched and every
#    citation there was still correct. The gate blocked a correct commit
#    because of a peer session's unstaged edit, and the cheapest ways out were
#    the bypass flag or rewriting the doc to cite line numbers that would be
#    FALSE for the commit being made. A gate whose own pressure pushes toward
#    writing a false citation is a fail-open with extra steps.
# ---------------------------------------------------------------------------

# SRC_LONG with 13 blank lines prepended: the needle moves 30 -> 43, i.e. 13
# lines, just past LINE_DRIFT_TOLERANCE (10). This is the reproduction shape.
SRC_LONG_SHIFTED = ("\n" * 13) + SRC_LONG

# The mirror: the needle sits at line 5 instead of line 30.
SRC_LONG_NEEDLE_EARLY = "".join(
    ('const NEEDLE: &str = "drifted marker";\n' if i == 5 else f"// line {i}\n")
    for i in range(1, 41)
)


class IndexIsTheJudgedTree(GateTestCase):
    """C1 + C2a + C2c: the default source is the index, and an unstaged edit
    on EITHER side (cited file or document) is not part of the verdict."""

    def _drifted_worktree_fixture(self) -> TempTree:
        """Doc and cited file both correct AS STAGED; the working-tree copy of
        the cited file has an unstaged 13-line shift (the measured defect)."""
        tree = self.with_doc(f"See `{PATH_LONG}:30` 「drifted marker」.\n")
        tree.write_unstaged(PATH_LONG, SRC_LONG_SHIFTED)
        return tree

    def test_unstaged_line_shift_in_a_cited_file_does_not_block_in_index_mode(self):
        """C2a, the reported defect. WOULD CATCH: reading the cited file with
        plain `open()`/`read_text()` instead of `git show :<path>` — i.e. any
        implementation that still consults the working tree for sources while
        claiming to judge the index."""
        tree = self._drifted_worktree_fixture()
        self.assertClean(*self.run_gate(tree.root, "--source", "index"))

    def test_the_default_source_is_the_index(self):
        """C1: `.githooks/pre-commit` invokes this gate with NO new argument, so
        the default MUST be `index` or the fix ships switched off. WOULD CATCH:
        `default="worktree"` on the `--source` argument (the mutation that makes
        every other test in this class pass while the real hook stays broken)."""
        tree = self._drifted_worktree_fixture()
        self.assertClean(*self.run_gate(tree.root))

    def test_control_the_same_fixture_still_blocks_in_worktree_mode(self):
        """The CONTROL for the two tests above: this fixture really does contain
        a >10-line drift, so `clean` in index mode is a statement about WHICH
        artifact was read, not about the fixture being empty. WOULD CATCH: an
        implementation that made `--source worktree` an alias of index (or that
        broke drift detection outright), which would make C2a vacuous."""
        tree = self._drifted_worktree_fixture()
        rc, out, err = self.run_gate(tree.root, "--source", "worktree")
        self.assertBlocks(rc, out, err, kind="line-drifted")
        self.assertIn(
            "43",
            out + err,
            "the worktree copy really does hold the quote 13 lines further down\n"
            f"STDOUT:{out}\nSTDERR:{err}",
        )

    def test_unstaged_edit_to_the_document_is_not_judged_in_index_mode(self):
        """C2c, the mirror direction. The STAGED doc cites a correct line; an
        unstaged edit to the same doc cites line 99 of a 5-line file. WOULD
        CATCH: reading documents from the working tree while reading sources
        from the index — the 'one source, consistently' property broken on the
        DOC side, which C2a alone cannot detect."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        tree.write_unstaged("docs/test.md", f"See `{PATH_A}:99`.\n")
        self.assertClean(*self.run_gate(tree.root, "--source", "index"))

    def test_control_the_unstaged_doc_edit_blocks_in_worktree_mode(self):
        """CONTROL for C2c: the unstaged doc really is wrong, so index mode's
        exit 0 is about the artifact read. WOULD CATCH: a fixture bug where
        `write_unstaged` accidentally staged, making the C2c test vacuous."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        tree.write_unstaged("docs/test.md", f"See `{PATH_A}:99`.\n")
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "worktree"), kind="line-out-of-range"
        )

    def test_a_doc_deleted_from_the_worktree_but_present_in_the_index_is_scanned(self):
        """C3: the commit will still record this document, so its claims are
        still the commit's claims. WOULD CATCH: enumerating from the index but
        then `open()`ing each doc from disk — which would raise/skip here and
        let a deleted-but-staged doc smuggle a false citation into the commit."""
        tree = self.with_doc("Cites `src/gone.rs:1`.\n")
        tree.delete_worktree_copy("docs/test.md")
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "index"), kind="path-not-found"
        )


class AntiVacuityStagedStalenessStillBlocks(GateTestCase):
    """C2b + C2d. Without these, an implementation that simply returned `clean`
    in index mode would pass every test in IndexIsTheJudgedTree. These are the
    tests that make `clean` mean something."""

    def test_a_cited_file_fixed_only_in_the_worktree_still_blocks(self):
        """C2b. The INDEX copy of the cited file is stale (needle at line 30,
        doc cites 5); the fix exists only as an unstaged working-tree edit.
        WOULD CATCH: an index mode that falls back to the working tree when the
        index and the tree differ — i.e. 'launder the commit with a fix you did
        not stage', the exact inverse of the defect being fixed."""
        tree = self.with_doc(f"See `{PATH_LONG}:5` 「drifted marker」.\n")
        tree.write_unstaged(PATH_LONG, SRC_LONG_NEEDLE_EARLY)
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "index"), kind="line-drifted"
        )

    def test_control_the_unstaged_fix_does_make_worktree_mode_clean(self):
        """CONTROL for C2b: proves the working-tree copy really is the 'fixed'
        one, so index mode's exit 1 is not just 'this fixture is broken
        everywhere'. WOULD CATCH: a fixture where the unstaged write never
        landed, which would make the C2b test pass for the wrong reason."""
        tree = self.with_doc(f"See `{PATH_LONG}:5` 「drifted marker」.\n")
        tree.write_unstaged(PATH_LONG, SRC_LONG_NEEDLE_EARLY)
        self.assertClean(*self.run_gate(tree.root, "--source", "worktree"))

    def test_a_document_corrected_only_in_the_worktree_still_blocks(self):
        """C2d. The STAGED doc cites line 99 of a 5-line file; the correction
        exists only unstaged. WOULD CATCH: an index mode that reads docs from
        the worktree — the C2c mutation in its permissive direction, which C2c
        (an exit-0 assertion) cannot detect on its own."""
        tree = self.with_doc(f"See `{PATH_A}:99`.\n")
        tree.write_unstaged("docs/test.md", f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "index"), kind="line-out-of-range"
        )

    def test_control_the_unstaged_correction_does_make_worktree_mode_clean(self):
        """CONTROL for C2d. WOULD CATCH: the same fixture bug as above, on the
        doc side."""
        tree = self.with_doc(f"See `{PATH_A}:99`.\n")
        tree.write_unstaged("docs/test.md", f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        self.assertClean(*self.run_gate(tree.root, "--source", "worktree"))

    def test_index_mode_still_detects_every_finding_kind(self):
        """The broadest anti-vacuity pin: all five slugs must still be reachable
        when the source is the index. WOULD CATCH: an index path that only ever
        produced `path-not-found` (or nothing), which would look 'fail-closed'
        while having stopped checking quotes and line numbers entirely."""
        cases = [
            ("path-not-found", "See `src/gone.rs:3`.\n"),
            ("line-out-of-range", f"See `{PATH_A}:99`.\n"),
            ("quote-not-found", f"See `{PATH_A}:3` 「fn goodbye() {{」.\n"),
            ("line-drifted", f"See `{PATH_LONG}:5` 「drifted marker」.\n"),
        ]
        for kind, body in cases:
            with self.subTest(kind=kind):
                tree = self.with_doc(body)
                rc, payload, err = self.json_of(tree.root, "--source", "index")
                self.assertEqual(rc, 1, f"{payload}\n{err}")
                self.assertEqual(
                    [f["kind"] for f in payload["findings"]],
                    [kind],
                    f"expected exactly {kind} in index mode: {payload}",
                )


class IndexModeDocSetEnumeration(GateTestCase):
    """C3: the SCOPE follows the source too. A scope enumerated from the wrong
    artifact re-introduces the same class of defect one level up."""

    def test_an_unstaged_doc_is_not_scanned_in_index_mode(self):
        """The scope half of the reported defect: a peer session's unstaged
        `docs/rogue.md` full of broken citations is not part of the commit
        being made, so it must not block it. WOULD CATCH: enumerating the doc
        set with `glob.glob(repo/docs/**/*.md)` while reading contents from the
        index."""
        tree = self.with_doc(f"Good claim `{PATH_A}:3`.\n")
        tree.write_unstaged("docs/rogue.md", "Rogue cites `src/gone.rs:1`.\n")
        self.assertClean(*self.run_gate(tree.root, "--source", "index"))

    def test_control_the_unstaged_rogue_doc_blocks_in_worktree_mode(self):
        """CONTROL: the rogue doc really is present and really is broken.
        WOULD CATCH: a fixture that never wrote it, making the test above
        vacuous."""
        tree = self.with_doc(f"Good claim `{PATH_A}:3`.\n")
        tree.write_unstaged("docs/rogue.md", "Rogue cites `src/gone.rs:1`.\n")
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "worktree"), kind="path-not-found"
        )

    def test_nested_staged_docs_are_walked_recursively_in_index_mode(self):
        """`docs/a/b/c.md` counts, exactly as under the working-tree glob.
        WOULD CATCH: an index enumeration written as `git ls-files docs/*.md`
        (a FLAT pathspec), which would silently drop every nested document —
        the same invisible scope shrink the recursive glob was introduced to
        fix."""
        tree = self.make_tree()
        tree.write("docs/a/b/c.md", "Nested cites `src/gone.rs:1`.\n")
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "index"), kind="path-not-found"
        )

    def test_an_empty_INDEX_doc_set_is_undetermined_even_with_docs_on_disk(self):
        """The existing 'an empty scope is not a clean scope' rule, now measured
        against the index — and measured where the two artifacts DISAGREE, so it
        cannot pass by accident. WOULD CATCH: an index mode that falls back to
        the filesystem when `git ls-files` returns nothing, or that treats an
        empty index scope as exit 0."""
        tree = self.make_tree()
        tree.write_unstaged("docs/present.md", f"Correct `{PATH_A}:3`.\n")
        rc, out, err = self.run_gate(tree.root, "--source", "index")
        self.assertEqual(rc, 2, f"empty INDEX scope must be undetermined:{out}\n{err}")
        self.assertIn("undetermined", err.lower(), f"STDERR:{err}")

    def test_doc_flag_naming_an_unstaged_path_is_undetermined_in_index_mode(self):
        """`--doc` is an assertion by the caller that the document exists in the
        judged artifact. In index mode a working-tree-only file does not.
        WOULD CATCH: resolving `--doc` with `os.path.isfile` while reading the
        content from the index — which would then read an empty/absent blob and
        report a vacuous clean."""
        tree = self.with_doc(f"Good claim `{PATH_A}:3`.\n")
        tree.write_unstaged("docs/rogue.md", "Rogue cites `src/gone.rs:1`.\n")
        self.assertUndetermined(
            *self.run_gate(tree.root, "--doc", "docs/rogue.md", "--source", "index")
        )

    def test_doc_flag_naming_a_staged_path_restricts_the_set_in_index_mode(self):
        """The P half of the pair above: once the same path IS staged, `--doc`
        works normally. WOULD CATCH: an implementation that resolved `--doc` to
        Undetermined unconditionally in index mode (fail-closed to the point of
        being unusable, which gets the gate switched off)."""
        tree = self.with_doc(f"Good claim `{PATH_A}:3`.\n")
        tree.write("docs/bad.md", "Bad claim `src/gone.rs:1`.\n")
        self.assertBlocks(
            *self.run_gate(tree.root, "--doc", "docs/bad.md", "--source", "index"),
            kind="path-not-found",
        )
        self.assertClean(
            *self.run_gate(tree.root, "--doc", "docs/test.md", "--source", "index")
        )

    def test_claude_md_is_still_out_of_the_default_scope_in_index_mode(self):
        """C6: the docs/CLAUDE.md split survives the migration. WOULD CATCH: an
        index enumeration written as `git ls-files '*.md'`, which would sweep
        CLAUDE.md (and every other markdown file in the repo) back into this
        gate's scope."""
        tree = self.make_tree()
        tree.write("CLAUDE.md", "Root doc cites `src/gone.rs:1`.\n")
        tree.write("docs/ok.md", f"Docs cites `{PATH_A}:3`.\n")
        self.assertClean(*self.run_gate(tree.root, "--source", "index"))


class UntrackedCitedFile(GateTestCase):
    """C5: a cited path present on disk but ABSENT FROM THE INDEX is
    `path-not-found` (exit 1), NOT undetermined.

    This is a deliberate line, not an oversight. Index mode judges the tree the
    commit will record, and `git ls-files` answers 'is this path in that tree'
    definitively — so absence is a real ANSWER about the claim, preserving the
    module's documented asymmetry ('a cited file that is MISSING is an answer
    about the claim; a cited file that EXISTS BUT CANNOT BE READ is not an
    answer at all'). Calling it undetermined would be a false statement about
    the gate's own knowledge AND would point the author at the wrong repair
    (there is nothing unobservable here; the fix is to stage the file or fix
    the citation)."""

    def _fixture(self) -> TempTree:
        tree = self.with_doc("The new thing lives at `src/brandnew.rs:1`.\n")
        tree.write_unstaged("src/brandnew.rs", "fn brand_new() {}\n")
        return tree

    def test_untracked_cited_file_is_path_not_found_in_index_mode(self):
        """WOULD CATCH: an index mode that fell back to the filesystem for a
        path missing from the index (reporting a vacuous clean over a file the
        commit will not contain)."""
        tree = self._fixture()
        rc, payload, err = self.json_of(tree.root, "--source", "index")
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(
            [f["kind"] for f in payload["findings"]],
            ["path-not-found"],
            f"an untracked cited file is a real ANSWER about the claim: {payload}",
        )

    def test_untracked_cited_file_is_NOT_undetermined(self):
        """The asymmetry, asserted directly rather than implied. WOULD CATCH:
        the over-correction — routing every index lookup miss into Undetermined
        (exit 2), which would report 'I could not observe this' about something
        `git ls-files` answers definitively, and would hide the real repair."""
        tree = self._fixture()
        rc, out, err = self.run_gate(tree.root, "--source", "index")
        self.assertEqual(rc, 1, f"expected exit 1\nSTDOUT:{out}\nSTDERR:{err}")
        self.assertNotIn(
            "undetermined",
            (out + err).lower(),
            "an untracked cited file must not be reported as unobservable",
        )

    def test_control_the_same_fixture_is_clean_in_worktree_mode(self):
        """CONTROL: the file really is there and really is correct on disk, so
        the exit 1 above is about the INDEX. WOULD CATCH: a fixture typo in the
        cited path, which would make the test above pass trivially."""
        tree = self._fixture()
        self.assertClean(*self.run_gate(tree.root, "--source", "worktree"))

    def test_staging_the_cited_file_turns_the_finding_green(self):
        """F -> P on one fixture: exit 1 while untracked, exit 0 once staged,
        with nothing else changed. WOULD CATCH: an implementation that reports
        `path-not-found` for EVERY cited path in index mode (a fail-closed that
        blocks every commit and therefore gets bypassed)."""
        tree = self._fixture()
        rc_before, out_b, err_b = self.run_gate(tree.root, "--source", "index")
        self.assertEqual(rc_before, 1, f"F half\nSTDOUT:{out_b}\nSTDERR:{err_b}")
        tree.stage("src/brandnew.rs")
        self.assertClean(*self.run_gate(tree.root, "--source", "index"))

    def test_a_staged_then_unstaged_cited_file_goes_back_to_path_not_found(self):
        """`git rm --cached` (path removed from the commit, kept on disk) is the
        same state arrived at from the other direction. WOULD CATCH: an index
        lookup cached/short-circuited by the filesystem's `isfile`."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        self.assertClean(*self.run_gate(tree.root, "--source", "index"))
        tree.unstage(PATH_A)
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "index"), kind="path-not-found"
        )


class IndexModeUndetermined(GateTestCase):
    """C4: every way index mode can FAIL TO OBSERVE resolves to exit 2 with a
    greppable `undetermined` — never exit 0, and never a finding that would
    read as 'checked'."""

    def _non_git_tree(self) -> Path:
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        root = Path(stack.name).resolve() / "plain"
        (root / "docs").mkdir(parents=True)
        (root / "src").mkdir(parents=True)
        (root / "src" / "a.rs").write_text(SRC_A, encoding="utf-8")
        (root / "docs" / "d.md").write_text(
            f"See `{PATH_A}:3` 「fn hello() {{」.\n", encoding="utf-8"
        )
        return root

    def test_a_non_git_repo_is_undetermined_in_index_mode(self):
        """There is no index to judge, so there is no verdict. WOULD CATCH: an
        implementation that swallows the `git` failure and falls back to the
        working tree — which is precisely the fail-open shape this change
        exists to remove, reintroduced as a 'graceful degradation'."""
        root = self._non_git_tree()
        self.assertUndetermined(*self.run_gate(root, "--source", "index"))

    def test_control_the_same_non_git_tree_is_clean_in_worktree_mode(self):
        """CONTROL: the tree's claims really are correct, so exit 2 above is
        about the missing index and not about a broken fixture. WOULD CATCH: a
        fixture whose doc was malformed, making the test above pass for the
        wrong reason."""
        root = self._non_git_tree()
        self.assertClean(*self.run_gate(root, "--source", "worktree"))

    def test_non_git_repo_json_is_undetermined_with_empty_findings(self):
        """C4's JSON half: `verdict == undetermined` AND `findings == []`.
        WOULD CATCH: emitting `verdict: clean` (or omitting the verdict) on the
        undetermined path, which downstream reads as 'checked and fine'."""
        root = self._non_git_tree()
        rc, payload, err = self.json_of(root, "--source", "index")
        self.assertEqual(rc, 2, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "undetermined", f"{payload}")
        self.assertEqual(payload["findings"], [], f"{payload}")

    def test_a_subdirectory_of_a_git_repo_is_undetermined_in_index_mode(self):
        """Index paths are resolved against the repository TOP LEVEL, so
        judging from a subdirectory would misresolve every cited path — and it
        would do so silently, reporting on files nobody named. WOULD CATCH: an
        implementation that runs `git -C <repo> ls-files` without checking that
        `<repo>` IS the top level (the fixture below is deliberately a valid
        self-consistent doc tree one level down, so a naive implementation
        reports a confident exit 0).

        THE STDERR ASSERTION IS LOAD-BEARING, and it was added after a measured
        vacuity rather than for tidiness. Deleting the top-level guard was a
        SURVIVING mutation against the exit-code assertion alone: with the
        guard gone, `ls-files` run from `sub/` yields subdirectory-relative
        paths, `:0:docs/d.md` then misses at the top level, and `cat-file`
        fails -- so the run still exits 2, via a DIFFERENT guard, and the test
        went on passing over a gate that had lost the check it was written for.
        Pinning WHICH refusal was reported is what separates the two."""
        tree = self.make_tree(sources={})
        tree.write("sub/src/a.rs", SRC_A)
        tree.write("sub/docs/d.md", f"See `src/a.rs:3` 「fn hello() {{」.\n")
        rc, out, err = self.run_gate(tree.root / "sub", "--source", "index")
        self.assertUndetermined(rc, out, err)
        self.assertIn(
            "top level",
            err.lower(),
            "exit 2 must be the TOP-LEVEL refusal, not some later guard "
            f"tripping over paths that were already misresolved\nSTDERR:{err}",
        )

    def test_a_subdirectory_whose_paths_are_shadowed_at_the_top_level(self):
        """The same rule where it actually fails OPEN, and the reason the plain
        case above is not enough on its own.

        The repository carries BOTH `sub/docs/d.md` + `sub/src/a.rs` AND
        top-level `docs/d.md` + `src/a.rs`, with DIFFERENT content: the
        subdirectory document makes a citation, the top-level one makes none.
        Now every subdirectory-relative path RESOLVES at the top level, so the
        backstops that masked the missing guard in the plain case are gone.

        WOULD CATCH: removing the "--repo must be the git top level" guard.
        Measured with that deletion applied, this fixture returns exit 0 and
        `all cited claims match the index` -- the gate answering confidently
        about a completely different pair of files from the ones the caller
        named. That is the worst shape a gate can take: not a missed finding,
        but a green verdict about the wrong tree."""
        tree = self.make_tree(sources={})
        # Top level: same relative paths, different content, NO citation at all.
        tree.write("src/a.rs", "fn shadowed() {\n}\n")
        tree.write("docs/d.md", "Top-level prose. Nothing is cited here.\n")
        # The subdirectory the caller will (wrongly) point the gate at.
        tree.write("sub/src/a.rs", SRC_A)
        tree.write("sub/docs/d.md", f"See `src/a.rs:3` 「fn hello() {{」.\n")
        rc, out, err = self.run_gate(tree.root / "sub", "--source", "index")
        self.assertUndetermined(rc, out, err)
        self.assertIn(
            "top level",
            err.lower(),
            f"the refusal must name the top-level rule\nSTDERR:{err}",
        )

    def test_git_that_cannot_be_executed_at_all_is_undetermined(self):
        """`OSError` on spawn (a PATH with no git). WOULD CATCH: an
        `except OSError: pass`-shaped fallback to the working tree — the
        canonical fail-open of this repo's 60 measured cases."""
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        empty_bin = Path(stack.name) / "bin"
        empty_bin.mkdir()
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        env = dict(ENV)
        env["PATH"] = str(empty_bin)
        proc = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--repo",
                str(tree.root),
                "--source",
                "index",
            ],
            capture_output=True,
            text=True,
            env=env,
        )
        self.assertUndetermined(proc.returncode, proc.stdout, proc.stderr)

    def test_a_git_invocation_that_exits_non_zero_is_undetermined(self):
        """Exit status is part of the answer: a checker that FELL OVER is not a
        checker that PASSED. WOULD CATCH: reading `git`'s stdout and ignoring
        its returncode — the recurring 'exit-code-ignored' fail-open, whose
        index-mode form would be an empty stdout read as an empty (therefore
        clean, or therefore missing) file list."""
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        shim_bin = Path(stack.name) / "bin"
        shim_bin.mkdir()
        shim = shim_bin / "git"
        shim.write_text("#!/bin/sh\nexit 1\n", encoding="utf-8")
        shim.chmod(0o755)
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        env = dict(ENV)
        env["PATH"] = str(shim_bin) + os.pathsep + env["PATH"]
        proc = subprocess.run(
            [
                sys.executable,
                str(SCRIPT),
                "--repo",
                str(tree.root),
                "--source",
                "index",
            ],
            capture_output=True,
            text=True,
            env=env,
        )
        self.assertUndetermined(proc.returncode, proc.stdout, proc.stderr)

    def _selective_git_shim(self, failing_subcommand: str, code: int = 3) -> Path:
        """A `git` earlier on PATH that fails ONE subcommand and execs the real
        binary for every other. Returns the directory to prepend to PATH.

        A blanket `exit 1` shim (the test above) cannot reach most call sites:
        the FIRST git invocation fails, the run aborts there, and every later
        site goes unmeasured. Faulting one subcommand at a time is what makes
        each call site individually observable."""
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        bindir = Path(stack.name) / "bin"
        bindir.mkdir()
        real_git = shutil.which("git", path=ENV["PATH"])
        self.assertIsNotNone(real_git, "the fixture needs a real git to delegate to")
        shim = bindir / "git"
        shim.write_text(
            "#!/bin/sh\n"
            'for a in "$@"; do\n'
            f'  if [ "$a" = "{failing_subcommand}" ]; then\n'
            f'    echo "shim: refusing {failing_subcommand}" >&2\n'
            f"    exit {code}\n"
            "  fi\n"
            "done\n"
            f'exec {real_git} "$@"\n',
            encoding="utf-8",
        )
        shim.chmod(0o755)
        return bindir

    def _run_with_path(self, repo, bindir, *extra):
        env = dict(ENV)
        if bindir is not None:
            env["PATH"] = str(bindir) + os.pathsep + env["PATH"]
        proc = subprocess.run(
            [sys.executable, str(SCRIPT), "--repo", str(repo), *extra],
            capture_output=True,
            text=True,
            env=env,
        )
        return proc.returncode, proc.stdout, proc.stderr

    def test_a_failing_cat_file_is_undetermined(self):
        """The blob-read call site, faulted on its own.

        This is the ONLY git call site in index mode with no backstop, and
        that is exactly why it needs its own test. Measured: deleting the
        `returncode != 0 -> Undetermined` check SURVIVED every other test in
        this suite, because when `rev-parse` fails the top-level guard raises
        instead, and when `ls-files` fails the empty-scope guard raises
        instead. Only `cat-file` has nothing behind it -- a document whose blob
        read silently returns "" parses to ZERO claims and reads as clean.

        WOULD CATCH: ignoring `git cat-file`'s exit status. With that deletion
        applied this fixture returns exit 0 and `all cited claims match the
        index` over a repository the gate could not read a single byte of --
        the gate reporting green precisely because it stopped reading
        anything, which is the fail-open this repo names as its worst act."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        bindir = self._selective_git_shim("cat-file")
        rc, out, err = self._run_with_path(tree.root, bindir, "--source", "index")
        self.assertUndetermined(rc, out, err)

    def test_control_the_same_fixture_is_clean_with_no_shim(self):
        """CONTROL for the test above: the repository really is healthy and
        really does verify, so the exit 2 is attributable to the faulted
        `cat-file` and not to a broken fixture. WOULD CATCH: a shim directory
        that leaked into the control, or a fixture whose claim was wrong all
        along -- either of which would make the kill test vacuous."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        self.assertClean(*self._run_with_path(tree.root, None, "--source", "index"))

    def test_a_failing_ls_files_is_undetermined(self):
        """The enumeration call site, faulted on its own. Today the
        empty-scope rule backstops this, but the backstop is incidental: it
        holds only while an unreadable file list happens to look like an empty
        one. WOULD CATCH: an enumeration that reports the failure as an empty
        doc set AND a future relaxation of the empty-scope rule -- the pair
        that would otherwise have to fail together to be noticed."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        bindir = self._selective_git_shim("ls-files")
        rc, out, err = self._run_with_path(tree.root, bindir, "--source", "index")
        self.assertUndetermined(rc, out, err)

    def test_a_failing_rev_parse_is_undetermined(self):
        """The top-level-identification call site, faulted on its own. Same
        argument as above: the top-level guard currently backstops it, because
        an unreadable answer compares unequal to the repo path. WOULD CATCH: an
        implementation that treats a failed `rev-parse` as 'no top level known,
        carry on', once that guard is the only thing holding."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        bindir = self._selective_git_shim("rev-parse")
        rc, out, err = self._run_with_path(tree.root, bindir, "--source", "index")
        self.assertUndetermined(rc, out, err)

    def test_a_cited_blob_that_is_not_utf8_in_the_index_is_undetermined(self):
        """The existing exists-but-cannot-be-read rule, moved to the index —
        and pinned where the artifacts DISAGREE (index blob is binary, the
        working-tree copy is perfectly readable), so it cannot pass by reading
        the wrong file. WOULD CATCH: decoding index blobs with
        `errors='replace'`/`errors='ignore'`, which turns an unreadable blob
        into a confidently checked one."""
        tree = self.make_tree()
        tree.write("src/bin.rs", "fn hello() {\n}\n")
        tree.stage_bytes("src/bin.rs", b"fn hello() {\n\xff\xfe\n}\n")
        tree.write("docs/d.md", "See `src/bin.rs:1` 「fn hello() {」.\n")
        self.assertUndetermined(*self.run_gate(tree.root, "--source", "index"))

    def test_control_that_same_fixture_is_clean_in_worktree_mode(self):
        """CONTROL for the test above: the working-tree copy is valid UTF-8 and
        the claim about it is true. WOULD CATCH: `stage_bytes` failing to leave
        the working tree alone, which would make the pair meaningless."""
        tree = self.make_tree()
        tree.write("src/bin.rs", "fn hello() {\n}\n")
        tree.stage_bytes("src/bin.rs", b"fn hello() {\n\xff\xfe\n}\n")
        tree.write("docs/d.md", "See `src/bin.rs:1` 「fn hello() {」.\n")
        self.assertClean(*self.run_gate(tree.root, "--source", "worktree"))

    def test_a_doc_blob_that_is_not_utf8_in_the_index_is_undetermined(self):
        """Same rule on the DOC side: a document in scope that cannot be decoded
        from the index is a failure to observe the scope, not an empty one.
        WOULD CATCH: an implementation that skips undecodable docs in index mode
        (silently shrinking the scope) instead of raising Undetermined."""
        tree = self.make_tree()
        tree.write("docs/d.md", f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        tree.stage_bytes("docs/d.md", b"# broken\n\xff\xfe\n")
        self.assertUndetermined(*self.run_gate(tree.root, "--source", "index"))

    def test_a_cited_path_staged_as_a_symlink_is_undetermined(self):
        """Index mode 120000. The index entry's CONTENT is the link target
        text, not the file it points at, so 'line 3 of src/link.rs' has no
        answer in the judged tree. WOULD CATCH: `git cat-file blob :<path>`
        with no mode check — which would silently verify the claim against the
        literal string `a.rs` (a 1-line 'file'), or, worse, resolve the link on
        the FILESYSTEM and check the working-tree file the commit does not
        record. The fixture is deliberately a claim that is TRUE through the
        symlink, so a naive implementation reports exit 0."""
        tree = self.make_tree()
        tree.add_symlink("src/link.rs", "a.rs")
        tree.write("docs/d.md", "See `src/link.rs:3` 「fn hello() {」.\n")
        self.assertUndetermined(*self.run_gate(tree.root, "--source", "index"))

    def test_a_cited_path_staged_as_a_gitlink_is_undetermined(self):
        """Index mode 160000 (a submodule). There is no blob to read — the
        entry is a commit id — so the claim cannot be checked. WOULD CATCH: the
        same missing mode check as above, in the direction where `cat-file
        blob` fails and an unchecked failure could be swallowed into `clean`.
        (The fixture directory carries a `.rs` suffix only so that the
        backticked token matches the gate's claim syntax.)"""
        tree = self.make_tree()
        tree.add_gitlink("vendored.rs")
        tree.write("docs/d.md", "See `vendored.rs:1` 「nested」.\n")
        self.assertUndetermined(*self.run_gate(tree.root, "--source", "index"))

    def test_an_unmerged_cited_path_is_undetermined(self):
        """A conflicted path has stages 1/2/3 and NO stage 0: there is no
        'what the commit would record' to read yet. WOULD CATCH: `git cat-file
        blob :<path>` (or `git show :<path>`) whose failure on an unmerged
        entry is swallowed, and — the permissive variant — an implementation
        that falls back to stage 2 ('ours') and reports a confident verdict
        about a tree that cannot be committed at all. The fixture builds the
        stages directly with `hash-object` + `update-index --index-info`, so
        no merge, branch or commit is involved and the state is deterministic."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        tree.make_unmerged(
            PATH_A,
            base="fn hello() {\n}\n",
            ours=SRC_A,
            theirs="fn hello() {\n    // theirs\n}\n",
        )
        self.assertUndetermined(*self.run_gate(tree.root, "--source", "index"))

    def test_control_the_unmerged_fixture_is_clean_in_worktree_mode(self):
        """The CONTROL for the test above, and a second INDEPENDENT
        demonstration that the worktree is not the artifact the verdict is
        about: during a conflict the working-tree copy is a file full of
        `<<<<<<<` / `=======` / `>>>>>>>` markers that cannot be committed at
        all, and worktree mode still reports exit 0 over it because the quoted
        line happens to survive inside one of the conflict hunks.

        WOULD CATCH: a `--source worktree` that had been quietly wired to the
        index (which would make the exit 2 above unattributable), and — read
        together with the test above — any argument that the working tree is
        the safer artifact to judge. Here it is the one that reports `clean`
        on a tree git will refuse to commit."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        tree.make_unmerged(
            PATH_A,
            base="fn hello() {\n}\n",
            ours=SRC_A,
            theirs="fn hello() {\n    // theirs\n}\n",
        )
        tree.write_unstaged(
            PATH_A,
            "<<<<<<< HEAD\n"
            "// header\n"
            "// second line\n"
            "fn hello() {\n"
            "    // swallow it and exit 0\n"
            "=======\n"
            "fn hello() {\n"
            "    // theirs\n"
            ">>>>>>> other\n"
            "}\n",
        )
        self.assertClean(*self.run_gate(tree.root, "--source", "worktree"))


class SourceFlagAndJson(GateTestCase):
    """C1 + C6: the flag exists, its value is auditable from `--json` alone, and
    an invalid value is an argparse error."""

    def test_help_documents_the_source_flag(self):
        """WOULD CATCH: shipping the index behaviour with no way to ask for the
        old one (and no discoverable name for the mode), which would leave the
        only escape hatch the gate-bypass ledger."""
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
        defaulting the unknown ones to `worktree` — a typo would then switch
        the gate back to the wrong artifact with no diagnostic."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        rc, out, err = self.run_gate(tree.root, "--source", "sideways")
        self.assertEqual(rc, 2, f"STDOUT:{out}\nSTDERR:{err}")

    def test_json_reports_index_as_the_source_used(self):
        """The mode must be auditable from the JSON alone — a reviewer reading a
        stored report cannot otherwise tell WHICH artifact produced `clean`.
        WOULD CATCH: omitting the `source` key, or hard-coding it to `index`
        while actually reading the working tree (see the paired test below)."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        rc, payload, err = self.json_of(tree.root, "--source", "index")
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload.get("source"), "index", f"{payload}")

    def test_json_reports_worktree_as_the_source_used(self):
        """The other half: a hard-coded `"source": "index"` would pass the test
        above and fail this one. WOULD CATCH exactly that mutation."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        rc, payload, err = self.json_of(tree.root, "--source", "worktree")
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload.get("source"), "worktree", f"{payload}")

    def test_json_default_run_reports_index_as_the_source(self):
        """C1 again, through the JSON: the DEFAULT is `index`. WOULD CATCH:
        `default="worktree"`, which no exit-code test on an agreeing fixture
        can see."""
        tree = self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n")
        rc, payload, err = self.json_of(tree.root)
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload.get("source"), "index", f"{payload}")

    def test_json_findings_keep_their_shape_in_index_mode(self):
        """C6: the finding record is unchanged by the migration. WOULD CATCH: a
        rewrite that renamed/dropped a key (e.g. reporting the blob sha instead
        of `path`), silently breaking every downstream reader."""
        tree = self.with_doc("# Title\n\nSee `src/gone.rs:7` 「fn hello() {」.\n")
        rc, payload, err = self.json_of(tree.root, "--source", "index")
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "mismatched")
        self.assertEqual(len(payload["findings"]), 1, f"{payload}")
        finding = payload["findings"][0]
        self.assertTrue(
            set(finding)
            >= {"doc", "doc_line", "path", "cited_line", "kind", "detail", "exempt"},
            f"finding is missing required keys: {finding}",
        )
        self.assertEqual(finding["doc"], "docs/test.md")
        self.assertEqual(finding["doc_line"], 3, "doc_line is 1-based")
        self.assertEqual(finding["path"], "src/gone.rs")
        self.assertEqual(finding["cited_line"], 7)
        self.assertEqual(finding["kind"], "path-not-found")
        self.assertIs(finding["exempt"], False)


class PreservedContractInBothModes(GateTestCase):
    """C6: everything that was true before must still be true, in BOTH modes.
    A migration that quietly relaxed the tolerance, dropped the exemption rule,
    or lost the escapes-repo check would 'fix' the reported defect by checking
    less."""

    MODES = (("default", ()), ("index", ("--source", "index")),
             ("worktree", ("--source", "worktree")))

    def test_drift_tolerance_is_still_exactly_ten_in_both_modes(self):
        """LINE_DRIFT_TOLERANCE == 10, inclusive: cited 20 (actual 30) clean,
        cited 19 (actual 30) blocks. WOULD CATCH: 'fixing' the reported
        false-block by widening the tolerance — which would make the gate
        tolerate real rot in every commit, in both modes, forever."""
        for name, flags in self.MODES:
            with self.subTest(source=name, drift=10):
                tree = self.with_doc(f"See `{PATH_LONG}:20` 「drifted marker」.\n")
                self.assertClean(*self.run_gate(tree.root, *flags))
            with self.subTest(source=name, drift=11):
                tree = self.with_doc(f"See `{PATH_LONG}:19` 「drifted marker」.\n")
                self.assertBlocks(
                    *self.run_gate(tree.root, *flags), kind="line-drifted"
                )

    def test_a_relative_path_escaping_the_repo_is_still_blocked_in_index_mode(self):
        """In index mode there is no filesystem to consult, so the check must be
        LEXICAL and must still fire. WOULD CATCH: dropping the escapes-repo
        branch in the index path (`git cat-file` would simply report the path as
        unknown, downgrading a capability finding into `path-not-found` — or,
        with a `-C` gone wrong, actually reading outside the repo)."""
        tree = self.make_tree()
        tree.write("docs/d.md", 'See `../outside/secret.txt:2` which says "TOKEN".\n')
        rc, payload, err = self.json_of(tree.root, "--source", "index")
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(
            [f["kind"] for f in payload["findings"]], ["path-escapes-repo"], f"{payload}"
        )

    def test_an_absolute_path_claim_is_path_escapes_repo_in_index_mode(self):
        """The absolute-path form of the same capability. WOULD CATCH: an index
        lookup that joins the claim onto the repo root with `os.path.join`
        (where an absolute second component silently WINS) and then hands
        the absolute path to the reader. The fixture cites a real file that
        really does contain the quoted line, so an implementation that reads it
        reports a confident exit 0 rather than an error."""
        tree = self.make_tree()
        outside = Path(tempfile.mkdtemp(prefix="outside_", dir=tree.base))
        self.addCleanup(shutil.rmtree, outside, True)
        secret = outside / "secret.txt"
        secret.write_text("TOKEN=hunter2\n", encoding="utf-8")
        tree.write("docs/d.md", f'See `{secret}:1` which says "TOKEN=hunter2".\n')
        rc, payload, err = self.json_of(tree.root, "--source", "index")
        self.assertEqual(rc, 1, f"{payload}\n{err}")
        self.assertEqual(
            [f["kind"] for f in payload["findings"]], ["path-escapes-repo"], f"{payload}"
        )

    def test_the_exemption_marker_still_works_in_index_mode(self):
        """C6: exact marker, one line before, mandatory reason, finding stays
        VISIBLE with exempt=true and the exit code is 0. WOULD CATCH: an index
        rewrite that lost the exemption plumbing (turning a documented
        historical citation into a permanent blocker) or that hid the exempted
        finding from `--json` (making the escape hatch unauditable)."""
        tree = self.with_doc(
            "<!-- doc-claim-exempt: upstream file, deliberately not vendored -->\n"
            "See `src/gone.rs:3` for the upstream shape.\n"
        )
        rc, payload, err = self.json_of(tree.root, "--source", "index")
        self.assertEqual(rc, 0, f"{payload}\n{err}")
        self.assertEqual(payload["verdict"], "clean")
        self.assertEqual(len(payload["findings"]), 1, f"{payload}")
        self.assertIs(payload["findings"][0]["exempt"], True, f"{payload}")

    def test_a_reasonless_exemption_still_exempts_nothing_in_index_mode(self):
        """WOULD CATCH: the loophole reappearing on the new code path."""
        tree = self.with_doc(
            "<!-- doc-claim-exempt: -->\nSee `src/gone.rs:3` for the shape.\n"
        )
        self.assertBlocks(
            *self.run_gate(tree.root, "--source", "index"), kind="path-not-found"
        )

    def test_no_new_exit_code_is_introduced(self):
        """C6: the exit contract stays {0,1,2}. WOULD CATCH: an implementation
        that invented a new code (say 3) for 'index unavailable', which
        `.githooks/pre-commit` — and every other caller that tests for 1 or 2 —
        would not recognise as a block."""
        seen = set()
        fixtures = [
            self.with_doc(f"See `{PATH_A}:3` 「fn hello() {{」.\n"),
            self.with_doc("See `src/gone.rs:1`.\n"),
            self.make_tree(),
        ]
        for tree in fixtures:
            for _name, flags in self.MODES:
                rc, _out, _err = self.run_gate(tree.root, *flags)
                seen.add(rc)
        self.assertTrue(
            seen <= {0, 1, 2}, f"exit codes must stay within {{0,1,2}}, saw {sorted(seen)}"
        )
        self.assertEqual(seen, {0, 1, 2}, f"all three must still be reachable: {seen}")


class RepoRootIdentification(GateTestCase):
    """The top-level guard must compare REAL PATHS, not strings.

    The guard that makes a subdirectory Undetermined is the same guard that
    decides whether a legitimate `--repo` is accepted at all, so its
    comparison has two failure directions and they need opposite fixtures.
    Too loose and a subdirectory is judged against the wrong files (pinned in
    IndexModeUndetermined); too tight -- a string comparison against
    `git rev-parse --show-toplevel` -- and the gate refuses REAL top-level
    repositories whose path is merely spelled differently, which on macOS is
    every `tempfile` path and on any platform is any symlinked checkout.

    A gate that reports Undetermined for a healthy repository is not 'safely
    fail-closed'. It blocks every commit, so it gets bypassed, and a bypassed
    gate detects nothing at all."""

    def _healthy_repo_at(self, root: Path) -> None:
        root.mkdir(parents=True, exist_ok=True)
        git(root, "init", "-q")
        (root / "src").mkdir(parents=True, exist_ok=True)
        (root / "src" / "a.rs").write_text(SRC_A, encoding="utf-8")
        (root / "docs").mkdir(parents=True, exist_ok=True)
        (root / "docs" / "d.md").write_text(
            f"See `{PATH_A}:3` 「fn hello() {{」.\n", encoding="utf-8"
        )
        git(root, "add", "--", "src/a.rs", "docs/d.md")

    def test_a_symlinked_repo_root_is_accepted(self):
        """`--repo` names the repository through a SYMLINK, so the string the
        caller passed can never equal `git rev-parse --show-toplevel`. This
        fixture is platform-independent: the two spellings are guaranteed to
        differ, so the pin cannot degenerate into a tautology anywhere.

        WOULD CATCH: comparing the two paths as strings (`top != repo`)
        instead of with `os.path.realpath` on BOTH sides. Under that
        implementation this healthy, fully staged repository reports exit 2
        -- the gate refusing to judge a tree it can read perfectly well."""
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        base = Path(stack.name).resolve()
        real = base / "real-repo"
        self._healthy_repo_at(real)
        link = base / "linked-repo"
        os.symlink(real, link)
        self.assertNotEqual(
            os.path.realpath(link),
            str(link),
            "fixture precondition: the link must not already be its own target",
        )
        self.assertClean(*self.run_gate(link, "--source", "index"))

    def test_an_unresolved_temp_dir_repo_root_is_accepted(self):
        """The case that actually bites on this machine: `tempfile` hands out
        `/var/folders/...` on macOS while git answers `/private/var/...`, so a
        string comparison rejects every temp repository -- and every developer
        checkout under a symlinked home.

        Note this test is only as strong as the platform: where `mkdtemp`
        already returns a real path the two spellings coincide and this
        degenerates into a duplicate of the symlink test above. That is why
        the symlink test exists and is the primary pin; this one is here
        because it is the exact shape the defect took when measured.

        WOULD CATCH: the same string comparison, on the path shape this
        repository's own fixtures use."""
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        unresolved = Path(stack.name) / "repo"  # deliberately NOT .resolve()
        self._healthy_repo_at(unresolved)
        self.assertClean(*self.run_gate(unresolved, "--source", "index"))

    def test_a_subdirectory_is_still_refused_through_a_symlink(self):
        """The other direction, so the fix for the tests above cannot be 'stop
        comparing at all'. A symlink pointing INTO the repository still names a
        subdirectory once resolved, and must still be refused.

        WOULD CATCH: repairing the string comparison by deleting the guard, or
        by comparing only basenames -- either of which would restore the
        judged-the-wrong-files fail-open that guard exists to prevent."""
        stack = tempfile.TemporaryDirectory()
        self.addCleanup(stack.cleanup)
        base = Path(stack.name).resolve()
        real = base / "real-repo"
        self._healthy_repo_at(real)
        (real / "sub" / "docs").mkdir(parents=True)
        (real / "sub" / "docs" / "d.md").write_text(
            f"See `{PATH_A}:3` 「fn hello() {{」.\n", encoding="utf-8"
        )
        git(real, "add", "--", "sub/docs/d.md")
        link = base / "into-repo"
        os.symlink(real / "sub", link)
        rc, out, err = self.run_gate(link, "--source", "index")
        self.assertUndetermined(rc, out, err)
        self.assertIn(
            "top level",
            err.lower(),
            f"the refusal must name the top-level rule\nSTDERR:{err}",
        )


if __name__ == "__main__":
    unittest.main(verbosity=2)
