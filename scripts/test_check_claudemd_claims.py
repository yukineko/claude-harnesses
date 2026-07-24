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

Every test builds a THROWAWAY repo tree under tempfile.TemporaryDirectory() and
invokes the script as a subprocess with `--repo <tmp>`; the real repository's
CLAUDE.md is never read or written by these tests.
"""

from __future__ import annotations

import json
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
SCRIPT = _HERE / "check-claudemd-claims.py"
ENGINE_SCRIPT = _HERE / "check-doc-claims.py"

ENV = {
    "LC_ALL": "C",
    "PATH": os.environ.get("PATH", "/usr/bin:/bin"),
    "PYTHONIOENCODING": "utf-8",
}

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
    """A disposable repo-shaped directory: CLAUDE.md + fake sources, no git."""

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


if __name__ == "__main__":
    unittest.main(verbosity=2)
