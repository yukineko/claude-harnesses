#!/usr/bin/env python3
"""Unit tests for scripts/check-projkey-store-classification.py (backlog 2c1e54b3).

THIS TEST FILE IS WRITTEN BEFORE THE GATE EXISTS (t1-classify, CLAUDE.md 2(a):
the test author and the implementer are different agents). Neither `scripts/check-projkey-store-classification.py`
nor `scripts/projkey-store-classification.toml` exists yet.

CORRECTION, added 2026-09-15 by the agent that rescued this file (NOT its
author, and NOT the implementer). The sentence here originally read "At the
time this file was committed ... see the commit message / task report for the
verbatim run". There is no such commit and no such task report: the authoring
run died before committing anything, and this file was recovered untracked
from an abandoned worktree. Leaving that sentence would have pointed the next
reviewer at evidence that does not exist (CLAUDE.md 4). The RED was instead
observed directly, at measurement point 03c82520 on 2026-09-15:

    python3 scripts/test_check_projkey_store_classification.py
    Ran 12 tests -- FAILED (failures=9)

so the RED is real, but it is the weak kind: it fails because the gate is
ABSENT, not because the gate decided wrongly.

THREE OF THESE TESTS ARE VACUOUSLY GREEN RIGHT NOW, and the implementer must
fix that rather than celebrate it:

    test_missing_table_file_exits_two
    test_corrupt_table_file_exits_two
    test_zero_consumers_discovered_exits_two_not_zero

All three assert `returncode == 2`, and an ABSENT script also exits 2 (the
interpreter's own "can't open file"). So they passed in the run above with no
gate on disk at all, and they would keep passing if the gate were deleted:
zero kill rate against the one mutation that matters most here. This is
exactly the failure mode CLAUDE.md 2 names -- a test that verifies nothing
always passes, and its assert then gets read as a specification. Before these
three can count as evidence, they need a discriminator that an absent script
cannot satisfy: assert on the gate's OWN diagnostic text on stderr, not on
the exit code alone. CLAUDE.md 2(b): a test never seen failing proves nothing, so
that RED observation is a required part of this ticket, not a formality.

WHAT THE GATE IS FOR. `harness_core::projkey::repo_root` stops at the first
ancestor whose `.git` is a directory ENTRY. In a linked worktree `.git` is a
FILE (git-worktree convention), so `repo_root` returns the worktree itself,
not the shared repo. Under CLAUDE.md 8 all condukt/flow work happens in linked
worktrees, so anything that keys a durable STORE off `repo_root`/`project_key`
silently partitions per worktree and reads back EMPTY — not "undetermined",
just quietly wrong (measured: 32 distinct `~/.condukt/state/<project>` dirs
for the one harness repo). This gate keeps a human-filled, machine-checked
table of every call site of `repo_root` / `project_key` / `main_worktree_root`
so a NEW, unclassified call site fails closed instead of shipping unnoticed.

TESTING STRATEGY. The contract this ticket hands to the implementer is a pure
CLI contract (env vars in, exit code + printed offender out) precisely so it
is testable without knowing the implementer's internal function names ahead
of time. So these tests are black-box: every test (except the real-repo
baseline) invokes the gate as a subprocess against a SYNTHETIC tree rooted at
a tempdir, via `PROJKEY_CLASS_ROOT` / `PROJKEY_CLASS_TABLE`. None of them
import the module under test, because there is no committed internal API to
import against — only the CLI contract in the ticket.

TWO ASSUMPTIONS this suite bakes in, flagged for the implementer/reviewer
because the ticket text does not pin them precisely:

  (a) "Call site" means the symbol name immediately followed by `(` — i.e. an
      actual invocation, not a mere mention. A `pub use harness_core::projkey
      ::{project_key, repo_root};` re-export line (no trailing paren) is NOT
      itself a call site. This matches the regex style already used by
      scripts/check-raw-io-ratchet.py (`\bread_to_string\s*\(`) and is what
      makes the re-export-hop fixture (test 10) resolvable without also
      forcing every `store.rs`-shaped re-export module into the table.
  (b) A `language = "gitignore"` entry's own validity does not require a
      `scope`/`kind` (those describe a Rust store *consumer*; a gitignore
      mechanism entry is a different creature per the ticket text). Only its
      `pattern` presence in the scanned tree's `.gitignore` is asserted here.

Both are judgment calls made explicit in code and in the task report, per
CLAUDE.md 2 ("judgment is prediction, not fact") — they are NOT silently
assumed to be the only correct reading.
"""

from __future__ import annotations

import os
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
REPO_ROOT = _HERE.parent
GATE_SCRIPT = _HERE / "check-projkey-store-classification.py"
REAL_TABLE = _HERE / "projkey-store-classification.toml"


def _write(path: Path, text: str) -> None:
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _toml_scalar(value) -> str:
    if isinstance(value, bool):
        return "true" if value else "false"
    if isinstance(value, int):
        return str(value)
    # Good enough for the plain ASCII strings this suite writes: TOML basic
    # strings escape backslash/quote exactly like this.
    escaped = str(value).replace("\\", "\\\\").replace('"', '\\"')
    return f'"{escaped}"'


def _write_table(path: Path, entries: list[dict]) -> None:
    lines = []
    for entry in entries:
        lines.append("[[consumer]]")
        for key, val in entry.items():
            lines.append(f"{key} = {_toml_scalar(val)}")
        lines.append("")
    _write(path, "\n".join(lines) + "\n")


def _env(root: Path | None = None, table: Path | None = None) -> dict:
    env = dict(os.environ)
    env.pop("PROJKEY_CLASS_ROOT", None)
    env.pop("PROJKEY_CLASS_TABLE", None)
    if root is not None:
        env["PROJKEY_CLASS_ROOT"] = str(root)
    if table is not None:
        env["PROJKEY_CLASS_TABLE"] = str(table)
    return env


def _run(env: dict, cwd: Path | None = None) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(GATE_SCRIPT)],
        cwd=str(cwd) if cwd is not None else None,
        env=env,
        capture_output=True,
        text=True,
        timeout=60,
    )


def _combined(result: subprocess.CompletedProcess) -> str:
    return (result.stdout or "") + (result.stderr or "")


class AllConsumersClassifiedPasses(unittest.TestCase):
    def test_single_classified_consumer_exits_zero(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write(
                root / "crates" / "fixturecrate" / "src" / "a.rs",
                'fn f() {\n    let r = harness_core::projkey::repo_root(&cwd);\n}\n',
            )
            table = root / "table.toml"
            _write_table(
                table,
                [
                    {
                        "file": "crates/fixturecrate/src/a.rs",
                        "symbol": "repo_root",
                        "count": 1,
                        "scope": "shared",
                        "kind": "store",
                        "evidence": "test fixture: single classified call site",
                    }
                ],
            )
            result = _run(_env(root, table))
            self.assertEqual(
                result.returncode, 0, msg=f"stdout={result.stdout!r} stderr={result.stderr!r}"
            )


class NewConsumerIsDetected(unittest.TestCase):
    """THE CENTRAL REQUIREMENT of the ticket: a new, unmentioned call site must
    fail the build rather than silently pass, and must name itself."""

    def test_new_consumer_not_in_table_exits_one_and_names_it(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write(
                root / "crates" / "fixturecrate" / "src" / "brand_new.rs",
                'fn f() {\n    let k = harness_core::projkey::project_key(&root);\n}\n',
            )
            table = root / "table.toml"
            _write_table(table, [])  # table knows about nothing
            result = _run(_env(root, table))
            self.assertEqual(result.returncode, 1)
            self.assertIn("brand_new.rs", _combined(result))


class ScopeValidationFailsClosed(unittest.TestCase):
    """CLAUDE.md 3: the script must never treat an absent/invalid scope as
    classified, and must never default a missing scope to anything (the
    "default to shared" rule in the ticket is for the HUMAN filling the
    table, not a fallback the script applies)."""

    def test_missing_scope_exits_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write(
                root / "crates" / "fixturecrate" / "src" / "a.rs",
                'fn f() {\n    let r = harness_core::projkey::repo_root(&cwd);\n}\n',
            )
            table = root / "table.toml"
            _write_table(
                table,
                [
                    {
                        "file": "crates/fixturecrate/src/a.rs",
                        "symbol": "repo_root",
                        "count": 1,
                        # scope deliberately omitted
                        "kind": "store",
                        "evidence": "scope missing on purpose",
                    }
                ],
            )
            result = _run(_env(root, table))
            self.assertEqual(result.returncode, 1)

    def test_bogus_scope_literal_exits_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write(
                root / "crates" / "fixturecrate" / "src" / "a.rs",
                'fn f() {\n    let r = harness_core::projkey::repo_root(&cwd);\n}\n',
            )
            table = root / "table.toml"
            _write_table(
                table,
                [
                    {
                        "file": "crates/fixturecrate/src/a.rs",
                        "symbol": "repo_root",
                        "count": 1,
                        "scope": "maybe",
                        "kind": "store",
                        "evidence": "scope is a bogus literal on purpose",
                    }
                ],
            )
            result = _run(_env(root, table))
            self.assertEqual(result.returncode, 1)


class UndeterminedInputsBlock(unittest.TestCase):
    """CLAUDE.md 3: cannot-determine must resolve to block (exit 2), and must
    be distinguishable from a positive violation finding (exit 1)."""

    def test_missing_table_file_exits_two(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write(
                root / "crates" / "fixturecrate" / "src" / "a.rs",
                'fn f() {\n    let r = harness_core::projkey::repo_root(&cwd);\n}\n',
            )
            table = root / "does-not-exist.toml"
            result = _run(_env(root, table))
            self.assertEqual(result.returncode, 2)

    def test_corrupt_table_file_exits_two(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write(
                root / "crates" / "fixturecrate" / "src" / "a.rs",
                'fn f() {\n    let r = harness_core::projkey::repo_root(&cwd);\n}\n',
            )
            table = root / "table.toml"
            _write(table, "[[consumer]\nfile = \"unterminated\n")  # malformed TOML
            result = _run(_env(root, table))
            self.assertEqual(result.returncode, 2)

    def test_zero_consumers_discovered_exits_two_not_zero(self):
        """An empty scan is NOT "clean" (CLAUDE.md 3: empty-set fail-open).
        We know the real repo has dozens of call sites; zero means the
        scanner itself is broken, and that must block, not pass."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            (root / "crates").mkdir(parents=True)  # tree exists, but no .rs files
            table = root / "table.toml"
            _write_table(table, [])  # well-formed but empty, to isolate this cause
            result = _run(_env(root, table))
            self.assertEqual(result.returncode, 2)
            self.assertNotEqual(result.returncode, 0)


class CountMismatchIsDetected(unittest.TestCase):
    def test_call_count_disagreement_exits_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write(
                root / "crates" / "fixturecrate" / "src" / "c.rs",
                "fn f() {\n"
                "    let a = harness_core::projkey::project_key(&p1);\n"
                "    let b = harness_core::projkey::project_key(&p2);\n"
                "}\n",
            )
            table = root / "table.toml"
            _write_table(
                table,
                [
                    {
                        "file": "crates/fixturecrate/src/c.rs",
                        "symbol": "project_key",
                        "count": 1,  # the file actually has 2 call sites
                        "scope": "shared",
                        "kind": "store",
                        "evidence": "deliberately wrong count",
                    }
                ],
            )
            result = _run(_env(root, table))
            self.assertEqual(result.returncode, 1)
            self.assertIn("c.rs", _combined(result))


class LookAlikeAndReexportDiscipline(unittest.TestCase):
    """Two structural gaps the ticket explicitly calls out for the scanner."""

    def test_store_project_key_lookalike_is_not_swept_in(self):
        """`harness_core::store::project_key` is a genuinely different
        function with a different (alnum-only, non-canonicalized) key scheme
        (see the warning at crates/harness-core/src/projkey.rs lines 13-14).
        If the scanner sweeps it in as a projkey consumer, that is a false
        positive that makes the gate unmaintainable.

        A genuine projkey consumer is included alongside the look-alike so
        this test isolates "the look-alike must not be counted" from the
        separate "zero consumers is undetermined" rule (UndeterminedInputs
        Block) — without it, a scanner that (correctly) finds nothing would
        exit 2, not the 0 this test wants to pin."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write(
                root / "crates" / "fixturecrate" / "src" / "legit.rs",
                'fn f() {\n    let r = harness_core::projkey::repo_root(&cwd);\n}\n',
            )
            _write(
                root / "crates" / "fixturecrate" / "src" / "lookalike.rs",
                'fn g() {\n    let k = harness_core::store::project_key(&x);\n}\n',
            )
            table = root / "table.toml"
            _write_table(
                table,
                [
                    {
                        "file": "crates/fixturecrate/src/legit.rs",
                        "symbol": "repo_root",
                        "count": 1,
                        "scope": "shared",
                        "kind": "store",
                        "evidence": "the one genuine consumer in this fixture",
                    }
                ],
            )
            result = _run(_env(root, table))
            self.assertEqual(
                result.returncode,
                0,
                msg=(
                    "harness_core::store::project_key must NOT be reported as a "
                    f"projkey consumer; stdout={result.stdout!r} stderr={result.stderr!r}"
                ),
            )

    def test_bare_call_after_local_reexport_is_detected(self):
        """condukt re-exports at crates/condukt/src/store.rs:9
        (`pub use harness_core::projkey::{project_key, repo_root};`), and
        downstream files call the BARE re-exported name after
        `use crate::store::{project_key, repo_root};`. Grepping for the
        fully-qualified path alone misses these entirely — this is the case
        the ticket explicitly warns about."""
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            _write(
                root / "crates" / "fixturecrate" / "src" / "store.rs",
                "pub use harness_core::projkey::{project_key, repo_root};\n",
            )
            _write(
                root / "crates" / "fixturecrate" / "src" / "consumer.rs",
                "use crate::store::{project_key, repo_root};\n\n"
                "fn f() {\n    let r = repo_root(&cwd);\n}\n",
            )
            table = root / "table.toml"
            _write_table(
                table,
                [
                    {
                        "file": "crates/fixturecrate/src/consumer.rs",
                        "symbol": "repo_root",
                        "count": 1,
                        "scope": "shared",
                        "kind": "store",
                        "evidence": "bare call via local re-export hop",
                    }
                ],
            )
            result = _run(_env(root, table))
            self.assertEqual(
                result.returncode,
                0,
                msg=(
                    "the bare repo_root(...) call reached via the local "
                    "crate::store re-export must be detected as a consumer; "
                    f"stdout={result.stdout!r} stderr={result.stderr!r}"
                ),
            )


class GitignoreMechanismEntries(unittest.TestCase):
    """On the compass side the classified mechanism is .gitignore lines
    21-22 (.compass/carve-state.json, .compass/outcomes.json), not a Rust
    call site — the table must span this, and must go stale-detecting if the
    pattern vanishes from the tree it is scanning."""

    def test_stale_gitignore_pattern_exits_one(self):
        with tempfile.TemporaryDirectory() as tmp:
            root = Path(tmp)
            # A genuine rust consumer keeps the "zero consumers" rule from
            # shadowing the assertion this test actually makes.
            _write(
                root / "crates" / "fixturecrate" / "src" / "a.rs",
                'fn f() {\n    let r = harness_core::projkey::repo_root(&cwd);\n}\n',
            )
            _write(root / ".gitignore", "target/\nnode_modules/\n")
            table = root / "table.toml"
            _write_table(
                table,
                [
                    {
                        "file": "crates/fixturecrate/src/a.rs",
                        "symbol": "repo_root",
                        "count": 1,
                        "scope": "shared",
                        "kind": "store",
                        "evidence": "genuine consumer, unrelated to the gitignore assertion",
                    },
                    {
                        "language": "gitignore",
                        "pattern": ".compass/carve-state.json",
                        "evidence": "table claims this pattern still exists; it does not",
                    },
                ],
            )
            result = _run(_env(root, table))
            self.assertEqual(result.returncode, 1)


class RealRepoBaseline(unittest.TestCase):
    """Once the implementer is done, the real tree with the real table must
    itself be clean. Deliberately the ONLY test in this suite that touches
    the real repo tree instead of a synthetic one."""

    def test_real_repo_with_real_table_exits_zero(self):
        result = _run(_env(), cwd=REPO_ROOT)
        self.assertEqual(
            result.returncode,
            0,
            msg=f"stdout={result.stdout!r} stderr={result.stderr!r}",
        )


if __name__ == "__main__":
    unittest.main()
