#!/usr/bin/env python3
"""Unit tests for scripts/check-gate-crates-sync.py.

Stdlib-only (`unittest`), no network. Exercises:
  1. the real repo state (all sources satisfy their required relation) -> exit 0.
  2. the relation-based design: canonical/exact/superset/mirror, including the
     "audit-only" crate (e.g. backlog) that continuous-audit.sh and the SKILL.md
     doc may carry beyond the canonical GATE_CRATES set.
  3. synthetic drift fixtures (one source violates its relation) -> exit 1, detected.
"""
import importlib.util
import os
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_gate_crates_sync", _HERE / "check-gate-crates-sync.py"
)
cgcs = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(cgcs)

REPO_ROOT = _HERE.parent

CANONICAL = "blastguard propguard specguard stuckguard mutategate overwatch"
CANONICAL_SET = set(CANONICAL.split())


def _write(path, text):
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(text, encoding="utf-8")


def _make_fixture_repo(
    tmp,
    *,
    pre_push_extra=(),
    pre_push_missing=(),
    ca_extra=(),
    ca_missing=(),
    hint_extra=(),
    hint_missing=(),
    skill_extra=(),
    skill_missing=(),
    condukt_extra=(),
    condukt_missing=(),
    tdd_extra=(),
    tdd_missing=(),
    condukt_comment="",
    tdd_comment="",
    hint_comment="",
    condukt_prefix="",
    tdd_prefix="",
    hint_prefix="",
):
    """Build a tmp dir with all 7 sources. By default everything is exactly in
    sync with CANONICAL. The `*_extra`/`*_missing` args perturb one source's
    crate set relative to CANONICAL (continuous-audit.sh/SKILL.md for the
    superset/mirror relations; pre-push/check-plugin-rollout.py and the two Rust
    constants for exact) so callers can exercise every relation, not just exact
    match. `hint_*` perturbs check-plugin-rollout.py's module-level GATE_CRATES
    tuple (the name is kept for continuity: that constant is what now generates
    the --canary drift hint).

    `condukt_comment` / `tdd_comment` / `hint_comment` inject a raw extra line
    INSIDE the literal's span (a Rust `//`/`/*…*/` or a Python `#` comment), so
    tests can pin that a commented-out or merely-mentioned crate name is NOT
    counted as a member.

    `condukt_prefix` / `tdd_prefix` / `hint_prefix` inject raw text BEFORE the
    constant (outside its span), so tests can pin the decoy case: a doc comment
    or docstring that restates the constant and is matched INSTEAD of it."""

    def apply(base, extra, missing):
        return (set(base) | set(extra)) - set(missing)

    pre_push_set = apply(CANONICAL_SET, pre_push_extra, pre_push_missing)
    ca_set = apply(CANONICAL_SET, ca_extra, ca_missing)
    hint_set = apply(CANONICAL_SET, hint_extra, hint_missing)
    skill_set = apply(CANONICAL_SET, skill_extra, skill_missing)
    condukt_set = apply(CANONICAL_SET, condukt_extra, condukt_missing)
    tdd_set = apply(CANONICAL_SET, tdd_extra, tdd_missing)

    _write(tmp / "scripts" / "rollout-plugins.sh", f'#!/bin/sh\nGATE_CRATES="{CANONICAL}"\n')
    _write(
        tmp / "scripts" / "continuous-audit.sh",
        f'#!/bin/sh\nDEFAULT_TARGETS="{",".join(sorted(ca_set))}"\n',
    )
    _write(
        tmp / ".githooks" / "pre-push",
        f"#!/bin/sh\nGATE_PATTERN='^crates/({'|'.join(sorted(pre_push_set))})/'\n",
    )
    # Reproduce the real file's shape: ONE module-level constant, with the
    # human-facing hint generated from it rather than repeated as a literal.
    _write(
        tmp / "scripts" / "check-plugin-rollout.py",
        hint_prefix
        + "GATE_CRATES = (\n"
        + "".join(f'    "{c}",\n' for c in sorted(hint_set))
        + (f"    {hint_comment}\n" if hint_comment else "")
        + ")\n\n\n"
        "def rollout_hint():\n"
        "    return f\"(add --canary for GATE crates: {'/'.join(GATE_CRATES)}).\"\n",
    )
    # Both Rust copies, in their two real shapes: condukt uses a sized array
    # (`[&str; N]`), tdd uses a slice (`&[&str]`). The extractor must handle both.
    _write(
        tmp / "crates" / "condukt" / "src" / "adversarial.rs",
        "/// The fleet GATE crates whose changes make a completion high-stakes.\n"
        + condukt_prefix
        + f"pub const GATE_CRATES: [&str; {len(condukt_set)}] = [\n"
        + "".join(f'    "{c}",\n' for c in sorted(condukt_set))
        + (f"    {condukt_comment}\n" if condukt_comment else "")
        + "];\n",
    )
    _write(
        tmp / "crates" / "tdd" / "src" / "config.rs",
        "/// Fleet gate crates: strict_separation defaults on for these.\n"
        + tdd_prefix
        + "pub const GATE_CRATES: &[&str] = &[\n"
        + "".join(f'    "{c}",\n' for c in sorted(tdd_set))
        + (f"    {tdd_comment}\n" if tdd_comment else "")
        + "];\n",
    )
    _write(
        tmp / "crates" / "overwatch" / "skills" / "continuous-audit" / "SKILL.md",
        "## 対象 crate (既定)\n\n"
        f"既定の target は fleet の **GATE crates**: `{','.join(sorted(skill_set))}`\n"
        "(同期の説明文)。`--target` で上書きできる。\n",
    )
    return tmp


def _by_path(parsed):
    return {rel_path: crates for rel_path, _mode, crates in parsed}


class RealRepoState(unittest.TestCase):
    def test_current_repo_is_in_sync(self):
        ok, canonical, parsed = cgcs.check(repo=str(REPO_ROOT))
        self.assertTrue(
            ok, f"repo GATE_CRATES sources drifted: canonical={canonical} parsed={parsed}"
        )

    def test_main_exits_zero_against_real_repo(self):
        cwd = os.getcwd()
        os.chdir(REPO_ROOT)
        try:
            rc = cgcs.main()
        finally:
            os.chdir(cwd)
        self.assertEqual(rc, 0)


class DriftDetection(unittest.TestCase):
    def test_fully_synced_fixture_passes(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp))
            ok, canonical, _ = cgcs.check(repo=str(repo))
            self.assertTrue(ok)
            self.assertEqual(canonical, CANONICAL_SET)

    def test_audit_only_addition_in_superset_and_mirror_passes(self):
        """continuous-audit.sh and SKILL.md both carry `backlog` beyond the
        canonical GATE_CRATES set (superset + mirror satisfied); pre-push
        stays GATE-crates-only (exact). This must PASS, not drift."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp), ca_extra=("backlog",), skill_extra=("backlog",)
            )
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertTrue(ok, f"expected audit-only addition to pass: {parsed}")
            by_path = _by_path(parsed)
            self.assertIn("backlog", by_path["scripts/continuous-audit.sh"])
            self.assertNotIn("backlog", by_path[".githooks/pre-push"])

    def test_pre_push_missing_a_crate_is_detected(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), pre_push_missing=("overwatch",))
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = _by_path(parsed)
            self.assertNotEqual(by_path[".githooks/pre-push"], canonical)
            self.assertNotIn("overwatch", by_path[".githooks/pre-push"])

    def test_continuous_audit_missing_a_canonical_crate_is_detected(self):
        """DEFAULT_TARGETS must be a superset of canonical; dropping a GATE
        crate violates the superset relation even with no extra crates."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), ca_missing=("overwatch",))
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = _by_path(parsed)
            self.assertFalse(canonical <= by_path["scripts/continuous-audit.sh"])

    def test_rollout_hint_missing_a_crate_is_detected(self):
        """Regression: check-plugin-rollout.py's GATE list shipped for a while
        listing only 5 of the 6 GATE crates (specguard was missing), telling the
        reader a plain rollout was fine for a crate rollout-plugins.sh rejects.
        Nothing caught it because it wasn't a tracked source. It is now."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), hint_missing=("specguard",))
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = _by_path(parsed)
            self.assertNotIn("specguard", by_path["scripts/check-plugin-rollout.py"])

    def test_rollout_hint_extra_crate_is_detected(self):
        """The hint is `exact`, not `superset`: naming a non-GATE crate would
        push someone into a needless canary rollout."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), hint_extra=("backlog",))
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            self.assertIn("backlog", _by_path(parsed)["scripts/check-plugin-rollout.py"])

    def test_rollout_const_unrecognizable_is_treated_as_drift(self):
        """If the constant is renamed/reshaped past recognition the extractor
        returns None. That must surface as drift (fail-closed + loud), never as a
        silent pass: an unparseable source is exactly the state where the check
        stops protecting anything, so it has to be noisy."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp))
            _write(
                repo / "scripts" / "check-plugin-rollout.py",
                'print("Fix: see the rollout docs for which crates need a canary.")\n',
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            self.assertIsNone(_by_path(parsed)["scripts/check-plugin-rollout.py"])

    def test_indented_prose_mention_does_not_shadow_the_real_constant(self):
        """What the extractor's `^` anchor actually pins: an INDENTED or
        mid-line `GATE_CRATES = (` inside prose is not a module-level constant
        and is skipped, so the real constant below is still what gets parsed."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp))
            path = repo / "scripts" / "check-plugin-rollout.py"
            path.write_text(
                '"""Docs: run with --canary for GATE crates: blastguard/propguard).\n'
                '    Historically a second GATE_CRATES = ("blastguard",) copy lived here.\n'
                '"""\n'
                + path.read_text(encoding="utf-8"),
                encoding="utf-8",
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertTrue(ok, f"indented prose shadowed the real constant: {parsed}")
            self.assertEqual(
                _by_path(parsed)["scripts/check-plugin-rollout.py"], CANONICAL_SET
            )

    def test_line_anchored_prose_copy_is_ambiguous_and_loud(self):
        """A decoy at column 0 inside a docstring is indistinguishable from the
        real constant to a regex. The old behaviour let the FIRST match win,
        which was loud only by luck — see the `_equal_to_canonical` tests below.
        Two candidate definitions must therefore be reported as unparseable
        (None) and drift, never resolved by position."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp))
            path = repo / "scripts" / "check-plugin-rollout.py"
            path.write_text(
                '"""Docs.\n'
                'GATE_CRATES = ("blastguard",)  # historical prose copy\n'
                '"""\n'
                + path.read_text(encoding="utf-8"),
                encoding="utf-8",
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok, "a shadowing line-anchored decoy must be LOUD")
            self.assertIsNone(
                _by_path(parsed)["scripts/check-plugin-rollout.py"],
                "two candidate definitions must not be silently resolved by "
                "picking the first one",
            )

    # ── decoys that RESTATE the constant (F2) ───────────────────────────────
    #
    # The dangerous shape is not a decoy that DIFFERS from canonical (loud by
    # accident) but one that EQUALS it while the live definition has drifted.
    # Then the checker reports the decoy's healthy set and prints a green OK for
    # a constant that actually lost a gate — a silent false negative in exactly
    # the drift this checker exists to catch. Both real Rust sources already
    # carry long "keep in sync with rollout-plugins.sh" doc comments directly
    # above their constant, so one illustrative example is all it takes.

    def test_rust_doc_comment_decoy_equal_to_canonical_is_not_silent(self):
        """condukt's array loses `overwatch` while the doc comment above it
        shows the full canonical list. Must be drift, not OK.

        The decoy is written on ONE line on purpose: the captured span then
        begins *after* the `[`, so it contains no `//` marker at all and
        stripping comments inside the span cannot see it. Only stripping BEFORE
        locating the constant removes it.
        """
        decoy = (
            "/// Keep in sync with scripts/rollout-plugins.sh, e.g.\n"
            "///   pub const GATE_CRATES: [&str; 6] = ["
            + ", ".join(f'"{c}"' for c in sorted(CANONICAL_SET))
            + "];\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp), condukt_missing=("overwatch",), condukt_prefix=decoy
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(
                ok, "a doc-comment decoy restating canonical silenced the gate"
            )
            self.assertNotIn(
                "overwatch",
                _by_path(parsed)["crates/condukt/src/adversarial.rs"] or set(),
            )

    def test_rust_block_comment_decoy_equal_to_canonical_is_not_silent(self):
        """Same decoy in `/* … */` form, above tdd's slice constant."""
        decoy = (
            "/* canonical reference:\n"
            "   pub const GATE_CRATES: &[&str] = &["
            + ", ".join(f'"{c}"' for c in sorted(CANONICAL_SET))
            + "];\n*/\n"
        )
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp), tdd_missing=("overwatch",), tdd_prefix=decoy
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(
                ok, "a block-comment decoy restating canonical silenced the gate"
            )
            self.assertNotIn(
                "overwatch", _by_path(parsed)["crates/tdd/src/config.rs"] or set()
            )

    def test_python_docstring_decoy_equal_to_canonical_is_not_silent(self):
        """The Python mirror: a column-0 docstring copy that EQUALS canonical
        while the live tuple lost `overwatch`. A comment cannot carry this shape
        (`^GATE_CRATES` can never match a `#`-prefixed line), so the docstring is
        the realistic decoy here — and equality makes it silent unless ambiguity
        itself is treated as unparseable."""
        decoy = (
            '"""Historical reference.\n'
            "GATE_CRATES = ("
            + ", ".join(f'"{c}"' for c in sorted(CANONICAL_SET))
            + ")\n"
            '"""\n'
        )
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp), hint_missing=("overwatch",), hint_prefix=decoy
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(
                ok, "a docstring decoy restating canonical silenced the gate"
            )
            self.assertNotIn(
                "overwatch",
                _by_path(parsed)["scripts/check-plugin-rollout.py"] or set(),
            )

    def test_python_comment_decoy_above_the_constant_is_not_counted(self):
        """A `#` comment above the tuple that names a crate the tuple does NOT
        contain must not be scraped as a member (comments are stripped from the
        whole file, not just from the captured span)."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp),
                hint_missing=("overwatch",),
                hint_prefix='# TODO: re-add "overwatch" here\n',
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok, "a comment above the tuple was counted as a member")
            self.assertNotIn(
                "overwatch",
                _by_path(parsed)["scripts/check-plugin-rollout.py"] or set(),
            )

    def test_hash_inside_a_single_quoted_python_string_is_not_a_comment(self):
        """`_strip_comments` documents that it leaves string literals intact, but
        it only knew DOUBLE-quoted ones: a `#` inside a single-quoted literal was
        read as a comment start and everything after it on that line was blanked.

        Pinned at the `_strip_comments` level because the damage is same-line
        only, and the GATE_CRATES literals happen to put one entry per line — so
        this is a contract violation that can only ever cause a (loud) false
        positive today. It is still a lie in the docstring, and the fix is free.
        """
        line = """SEP = '#'; KEEP = "blastguard"\n"""
        self.assertEqual(
            cgcs._strip_comments(line, cgcs._PYTHON_COMMENT, cgcs._PYTHON_STRING),
            line,
        )
        # And the triple-quoted form, which the same pattern must span.
        doc = '"""a docstring mentioning #hashtags"""\nKEEP = "blastguard"\n'
        self.assertIn(
            '"blastguard"',
            cgcs._strip_comments(doc, cgcs._PYTHON_COMMENT, cgcs._PYTHON_STRING),
        )

    def test_rust_constants_are_parsed_in_both_shapes(self):
        """condukt's sized array (`[&str; N]`) and tdd's slice (`&[&str]`) must
        both parse to the canonical set in the fully-synced fixture."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp))
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertTrue(ok, f"expected synced Rust constants to pass: {parsed}")
            by_path = _by_path(parsed)
            self.assertEqual(by_path["crates/condukt/src/adversarial.rs"], CANONICAL_SET)
            self.assertEqual(by_path["crates/tdd/src/config.rs"], CANONICAL_SET)

    def test_condukt_rust_const_missing_overwatch_is_detected(self):
        """The exact drift that shipped: condukt's adversarial panel constant
        lost `overwatch`, so changes to the Continuous-Audit crate never forced
        the panel. This is the hole the new source closes."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), condukt_missing=("overwatch",))
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            self.assertNotIn(
                "overwatch", _by_path(parsed)["crates/condukt/src/adversarial.rs"]
            )

    def test_tdd_rust_const_missing_overwatch_is_detected(self):
        """Same drift in tdd's copy: strict_separation stayed default-off inside
        crates/overwatch/**, letting one agent author both RED and GREEN there."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), tdd_missing=("overwatch",))
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            self.assertNotIn("overwatch", _by_path(parsed)["crates/tdd/src/config.rs"])

    def test_rust_const_extra_crate_is_detected(self):
        """The Rust copies are `exact`, not `superset`: an audit-only crate like
        `backlog` must not sneak in and force a panel / strict separation for a
        crate that gates nothing."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), tdd_extra=("backlog",))
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            self.assertIn("backlog", _by_path(parsed)["crates/tdd/src/config.rs"])

    def test_rust_const_unrecognizable_is_treated_as_drift(self):
        """A Rust copy that no longer parses fails closed, like the others."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp))
            _write(
                repo / "crates" / "condukt" / "src" / "adversarial.rs",
                "pub fn gate_crates() -> Vec<&'static str> { vec![\"blastguard\"] }\n",
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            self.assertIsNone(_by_path(parsed)["crates/condukt/src/adversarial.rs"])

    # ── commented-out entries must not be counted (F1) ──────────────────────
    #
    # Commenting a line out in place is the most natural way someone disables an
    # entry. If the extractor scrapes quoted names out of the raw span it counts
    # the comment's crate name as a live member: the constant loses a gate and
    # the checker still prints "OK". Both the commented-out-in-place form and a
    # prose TODO that merely names the crate must read as DRIFT.

    def test_rust_const_commented_out_entry_is_detected(self):
        """`// "overwatch",` left in place — the entry is gone from the array."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp),
                condukt_missing=("overwatch",),
                condukt_comment='// "overwatch",',
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok, "commented-out Rust entry was counted as live")
            self.assertNotIn(
                "overwatch", _by_path(parsed)["crates/condukt/src/adversarial.rs"]
            )

    def test_rust_const_line_comment_mention_is_detected(self):
        """A TODO naming the crate is not a member: the array really has 5."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp),
                tdd_missing=("overwatch",),
                tdd_comment='// TODO: re-add "overwatch" once panel cost is acceptable',
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok, "Rust line-comment mention was counted as live")
            self.assertNotIn("overwatch", _by_path(parsed)["crates/tdd/src/config.rs"])

    def test_rust_const_block_comment_mention_is_detected(self):
        """The `/* … */` form is stripped too, not just `//`."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp),
                condukt_missing=("overwatch",),
                condukt_comment='/* "overwatch", disabled for now */',
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok, "Rust block-comment mention was counted as live")
            self.assertNotIn(
                "overwatch", _by_path(parsed)["crates/condukt/src/adversarial.rs"]
            )

    def test_python_const_commented_out_entry_is_detected(self):
        """Same hole on the Python side: `# "overwatch",` inside the tuple."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp),
                hint_missing=("overwatch",),
                hint_comment='# "overwatch",',
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok, "commented-out Python entry was counted as live")
            self.assertNotIn(
                "overwatch", _by_path(parsed)["scripts/check-plugin-rollout.py"]
            )

    def test_python_const_comment_mention_is_detected(self):
        """A prose TODO naming the crate inside the tuple is not a member."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp),
                hint_missing=("overwatch",),
                hint_comment='# TODO: re-add "overwatch" once panel cost is acceptable',
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok, "Python comment mention was counted as live")
            self.assertNotIn(
                "overwatch", _by_path(parsed)["scripts/check-plugin-rollout.py"]
            )

    def test_comment_stripping_does_not_drop_live_entries(self):
        """The strip must remove only comments: a fixture whose literal carries a
        harmless comment alongside the full canonical set still parses to 6.

        The comments deliberately QUOTE a non-canonical crate name, so this test
        fails if `_strip_comments` ever becomes a no-op — the old version's
        comments contained no crate names at all and so passed either way.
        """
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(
                Path(tmp),
                condukt_comment='// keep "backlog" out — audit-only, not a gate',
                tdd_comment='/* keep "backlog" out — audit-only, not a gate */',
                hint_comment='# keep "backlog" out — audit-only, not a gate',
            )
            ok, _canonical, parsed = cgcs.check(repo=str(repo))
            self.assertTrue(ok, f"comment stripping ate live entries: {parsed}")
            by_path = _by_path(parsed)
            self.assertEqual(by_path["crates/condukt/src/adversarial.rs"], CANONICAL_SET)
            self.assertEqual(by_path["crates/tdd/src/config.rs"], CANONICAL_SET)
            self.assertEqual(
                by_path["scripts/check-plugin-rollout.py"], CANONICAL_SET
            )

    def test_skill_md_diverging_from_continuous_audit_is_detected(self):
        """SKILL.md must mirror continuous-audit.sh's DEFAULT_TARGETS exactly.
        continuous-audit.sh gains `backlog` but SKILL.md doesn't -> drift."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), ca_extra=("backlog",))
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = _by_path(parsed)
            self.assertNotEqual(
                by_path["crates/overwatch/skills/continuous-audit/SKILL.md"],
                by_path["scripts/continuous-audit.sh"],
            )

    def test_unparseable_mirror_target_does_not_blame_the_mirror(self):
        """When continuous-audit.sh cannot be parsed, SKILL.md (its mirror) must
        not be reported as carrying `unexpected [...]` — that blames the doc for
        the script's breakage and sends the reader to the wrong file. The old
        `by_path.get(...) or set()` turned an unparseable target into an empty
        set and did exactly that."""
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp))
            _write(repo / "scripts" / "continuous-audit.sh", "#!/bin/sh\n# no targets\n")
            ok, canonical, parsed = cgcs.check(repo=str(repo))
            self.assertFalse(ok)
            by_path = _by_path(parsed)
            detail = cgcs._mismatch_detail(
                by_path["crates/overwatch/skills/continuous-audit/SKILL.md"],
                "mirror:scripts/continuous-audit.sh",
                canonical,
                by_path,
            )
            self.assertNotIn("unexpected", detail)
            self.assertIn("scripts/continuous-audit.sh", detail)

    def test_main_exits_one_on_drifted_fixture(self):
        with tempfile.TemporaryDirectory() as tmp:
            repo = _make_fixture_repo(Path(tmp), pre_push_missing=("overwatch",))
            cwd = os.getcwd()
            os.chdir(repo)
            try:
                rc = cgcs.main()
            finally:
                os.chdir(cwd)
            self.assertEqual(rc, 1)


if __name__ == "__main__":
    unittest.main()
