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

Every test builds a THROWAWAY doc tree under tempfile.TemporaryDirectory() and
invokes the script as a subprocess with `--repo <tmp>`; the real repository's
docs and sources are never read or written by these tests.
"""

from __future__ import annotations

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
SCRIPT = _HERE / "check-doc-claims.py"

ENV = {
    "LC_ALL": "C",
    "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    "PYTHONIOENCODING": "utf-8",
}

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
    """A disposable repo-shaped directory: docs + fake sources, no git."""

    def __init__(self, stack: tempfile.TemporaryDirectory):
        self.root = Path(stack.name)

    def write(self, relpath: str, content: str) -> Path:
        p = self.root / relpath
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content, encoding="utf-8")
        return p

    def write_bytes(self, relpath: str, raw: bytes) -> Path:
        p = self.root / relpath
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_bytes(raw)
        return p


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


if __name__ == "__main__":
    unittest.main(verbosity=2)
