#!/usr/bin/env python3
"""Unit tests for scripts/check-fail-open.py (fail-open-guard).

Stdlib-only (`unittest`), no third-party dependency, so it runs identically in
CI and locally:  `python3 scripts/test_check_fail_open.py`.

Load-bearing properties pinned here:
  1. A planted swallow (read_dir let-else, read_dir `.flatten()`, or a shell
     `$(… 2>/dev/null)` capture whose rc vanishes) IS caught — the gate has teeth.
  2. False-positive discipline (or the gate gets disabled): an Option `.flatten()`,
     a shell capture whose rc IS recovered (same line OR next line), a `command -v`
     probe, a comment line, a `#[cfg(test)]` module, and the fail-CLOSED `match
     read_dir {…}` form are all NOT flagged.
  3. The live GATE surface scans clean (regression floor), so a new swallow is a
     NEW signal, not lost in noise.
  4. A file the scanner cannot read is a finding, never silently clean.
  5. The ALLOWLIST actually suppresses, and only for its own (path, pattern, needle).

The module under test has hyphens in its name, so it is loaded via importlib.
"""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open", _HERE / "check-fail-open.py"
)
fo = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(fo)


def names(hits):
    return {n for _, _, n in hits}


class DetectsPlantedSwallow(unittest.TestCase):
    def test_readdir_let_else(self):
        src = [
            "fn walk(dir: &Path) {",
            "    let Ok(entries) = std::fs::read_dir(dir) else {",
            "        return;",
            "    };",
            "}",
        ]
        self.assertIn("readdir-let-else-swallow", names(fo.scan_rust(src)))

    def test_readdir_flatten(self):
        src = [
            "let rd = std::fs::read_dir(dir).unwrap();",
            "for e in rd.flatten() {",
            "    let _ = e;",
            "}",
        ]
        self.assertIn("readdir-flatten-swallow", names(fo.scan_rust(src)))

    def test_shell_devnull_capture(self):
        src = ['changed="$(git diff --name-only 2>/dev/null)"', 'echo "$changed"']
        self.assertIn("shell-devnull-capture-swallow", names(fo.scan_shell(src)))


class FalsePositiveDiscipline(unittest.TestCase):
    def test_option_flatten_is_not_flagged(self):
        # No read_dir anywhere near → an Option/iterator flatten, not a walk.
        src = ["let first = maybe.and_then(|x| x).flatten();"]
        self.assertEqual(fo.scan_rust(src), [])

    def test_flatten_far_from_readdir_is_not_flagged(self):
        # read_dir far above (> window) → not correlated with this flatten.
        src = ["let rd = std::fs::read_dir(d).unwrap();"] + \
              ["let _ = 0;"] * 8 + ["let y = opt.flatten();"]
        self.assertEqual(fo.scan_rust(src), [])

    def test_match_readdir_failclosed_form_is_not_flagged(self):
        # The FIXED form (rounds #7/#11): no let-else, no flatten.
        src = [
            "let entries = match std::fs::read_dir(dir) {",
            "    Ok(e) => e,",
            "    Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),",
            "    Err(e) => return Err(e.into()),",
            "};",
            "for entry in entries {",
            "    match entry { Ok(e) => paths.push(e.path()), Err(e) => bail!(e) }",
            "}",
        ]
        self.assertEqual(fo.scan_rust(src), [])

    def test_shell_rc_recovered_same_line(self):
        self.assertEqual(fo.scan_shell(['x="$(cmd 2>/dev/null)" || rc=$?']), [])

    def test_shell_rc_recovered_next_line(self):
        self.assertEqual(fo.scan_shell(['x="$(cmd 2>/dev/null)"', "rc=$?"]), [])

    def test_shell_command_v_probe_not_flagged(self):
        self.assertEqual(fo.scan_shell(['P="$(command -v foo 2>/dev/null)"']), [])

    def test_shell_or_true_not_flagged(self):
        self.assertEqual(fo.scan_shell(['t="$(date +%s 2>/dev/null || true)"']), [])

    def test_comment_line_is_not_flagged(self):
        src = ["// let Ok(x) = std::fs::read_dir(d) else { return };"]
        self.assertEqual(fo.scan_rust(src), [])

    def test_cfg_test_module_is_excluded(self):
        src = [
            "fn prod() { let _ = 1; }",
            "#[cfg(test)]",
            "mod tests {",
            "    fn t() {",
            "        let Ok(entries) = std::fs::read_dir(d) else { return };",
            "        for e in entries.flatten() {}",
            "    }",
            "}",
        ]
        self.assertEqual(fo.scan_rust(src), [], "test-module swallows are out of scope")

    def test_production_before_cfg_test_is_still_scanned(self):
        # A swallow in production code that PRECEDES a test module must still fire
        # (the exclusion must not swallow production lines — a gate fail-open).
        src = [
            "fn walk(d: &Path) {",
            "    let Ok(entries) = std::fs::read_dir(d) else { return };",
            "    let _ = entries;",
            "}",
            "#[cfg(test)]",
            "mod tests { fn t() {} }",
        ]
        self.assertIn("readdir-let-else-swallow", names(fo.scan_rust(src)))


class GateSurfaceRegressionFloor(unittest.TestCase):
    def test_gate_surface_scans_clean(self):
        # `blocking_hits` (not raw `scan_file`) is the right expression of this
        # test's intent — "the merge-BLOCKING gate surface is clean" — now that
        # the empty-collection fallback class is scored on the --ratchet burn-down
        # instead of on this verdict. The companion test below asserts the
        # advisory class is genuinely non-empty here, so this filter can never
        # quietly become a way to report clean by detecting nothing.
        dirty = []
        for p in fo.iter_target_files(all_crates=False):
            hits = fo.blocking_hits(fo.scan_file(p))
            if hits:
                dirty.append((str(p.relative_to(fo.REPO)), hits))
        self.assertEqual(dirty, [], f"gate surface not clean: {dirty}")

    def test_the_advisory_filter_is_not_vacuous_on_the_gate_surface(self):
        # Anti-vacuity control for the filter above. The gate surface DOES hold
        # empty-collection fallbacks today (overwatch/src/store.rs alone has
        # several). If this ever reaches zero, the burn-down actually finished —
        # at which point the class should be promoted into the blocking verdict,
        # not left as a filter that hides nothing.
        advisory = 0
        for p in fo.iter_target_files(all_crates=False):
            hits = fo.scan_file(p)
            advisory += len(hits) - len(fo.blocking_hits(hits))
        self.assertGreater(
            advisory, 0,
            "the advisory class matched NOTHING on the gate surface — either the "
            "burn-down is complete (promote the class to blocking) or the "
            "patterns silently stopped matching",
        )

    def test_main_gate_surface_exit_zero(self):
        self.assertEqual(fo.main(["check-fail-open.py"]), 0)

    def test_empty_discovery_fails_closed_not_clean(self):
        # If file discovery comes up empty on the merge-blocking gate surface, the
        # scanner must NOT report "clean" (exit 0) — that would be the scanner
        # itself failing open. It must refuse (exit 1). Patch iter_target_files to
        # return [] (simulating a broken `git ls-files` / wrong cwd).
        saved = fo.iter_target_files
        try:
            fo.iter_target_files = lambda all_crates: []
            self.assertEqual(fo.main(["check-fail-open.py"]), 1,
                             "empty gate-surface discovery must fail closed")
            # …but --all is advisory (discovery-only), so empty there is exit 0.
            self.assertEqual(fo.main(["check-fail-open.py", "--all"]), 0)
        finally:
            fo.iter_target_files = saved


class UnreadableSourceIsNotClean(unittest.TestCase):
    def test_missing_file_is_a_finding(self):
        hits = fo.scan_file(fo.REPO / "does-not-exist-xyz.rs")
        self.assertTrue(any(n == "unreadable-source" for _, _, n in hits))


class AllowlistSuppression(unittest.TestCase):
    def test_every_allowlist_entry_suppresses_a_real_hit(self):
        """No dead suppression: each ALLOWLIST entry must be masking something
        that is actually there right now.

        Driven off ALLOWLIST itself rather than a hardcoded file, because the
        hardcoded form went stale silently: 43ec3e80 fixed the real swallows in
        `crates/overwatch/src/test_freshness.rs` and correctly dropped its two
        entries, but this test kept naming that file and started asserting that
        a swallow which no longer exists must reappear. It went red on 2026-07-21
        and stayed red for 6 consecutive CI runs, unnoticed because `fail-open.yml`
        was `paths:`-filtered and is not (yet) a required status check.

        A dead entry is not merely untidy — it is a live fail-open: it would
        silently suppress a swallow re-introduced at that (path, pattern, needle)
        later on.
        """
        self.assertTrue(fo.ALLOWLIST, "ALLOWLIST is empty — nothing to vouch for")
        for entry in fo.ALLOWLIST:
            p = fo.REPO / entry["path"]
            with self.subTest(path=entry["path"], pattern=entry["pattern"]):
                self.assertTrue(
                    p.exists(),
                    f"allowlist entry points at a missing file: {entry['path']}")
                # With the allowlist in force, this entry's pattern is suppressed.
                self.assertNotIn(entry["pattern"], names(fo.scan_file(p)),
                                 "allowlisted hit must be suppressed")
                # …and only because of the allowlist: emptied, the hit comes back.
                saved = fo.ALLOWLIST[:]
                try:
                    fo.ALLOWLIST.clear()
                    self.assertIn(
                        entry["pattern"], names(fo.scan_file(p)),
                        "without the allowlist the real hit must show — a "
                        "suppression that masks nothing is a dead entry and "
                        "must be deleted, not kept")
                finally:
                    fo.ALLOWLIST.clear()
                    fo.ALLOWLIST.extend(saved)

    def test_allowlist_needle_is_path_scoped(self):
        # The `entries.flatten()` needle must not suppress the same text in a
        # DIFFERENT file (allowlist keys on path too).
        f = tempfile.NamedTemporaryFile("w", suffix=".rs", delete=False)
        f.write("let rd = std::fs::read_dir(d).unwrap();\nfor e in entries.flatten() {}\n")
        f.close()
        hits = fo.scan_file(Path(f.name))
        self.assertIn("readdir-flatten-swallow", names(hits),
                      "allowlist must not suppress the same text in another file")


class RatchetVerdictPureLogic(unittest.TestCase):
    """`ratchet_verdict(count, baseline)` decision table (no IO).

    Discriminates regression (ROSE) from unlocked-improvement (FELL) from
    hold (==). Both drift directions must be exit 1 with DISTINCT messages;
    only equality is exit 0. RED on origin/main: `ratchet_verdict` does not
    exist there (AttributeError)."""

    def test_count_above_baseline_is_exit1_regression(self):
        code, msg = fo.ratchet_verdict(35, 34)
        self.assertEqual(code, 1)
        self.assertIn("ROSE", msg)
        self.assertNotIn("FELL", msg)

    def test_count_below_baseline_is_exit1_improvement(self):
        code, msg = fo.ratchet_verdict(33, 34)
        self.assertEqual(code, 1)
        self.assertIn("FELL", msg)
        self.assertNotIn("ROSE", msg)

    def test_count_equals_baseline_is_exit0_hold(self):
        code, msg = fo.ratchet_verdict(34, 34)
        self.assertEqual(code, 0)
        self.assertIn("baseline", msg)
        # An equal count is neither a rise nor a fall.
        self.assertNotIn("ROSE", msg)
        self.assertNotIn("FELL", msg)

    def test_messages_distinguish_the_two_drift_directions(self):
        _, up = fo.ratchet_verdict(40, 34)
        _, down = fo.ratchet_verdict(30, 34)
        self.assertNotEqual(up, down,
                            "regression and improvement must not share a message")


class ReadBaselineFailClosed(unittest.TestCase):
    """`read_baseline` must RAISE BaselineError on any cannot-determine — never
    silently return 0 (a bogus floor reads every count as a regression, or an
    improvement-to-zero). RED on origin/main: `read_baseline`/`BaselineError`
    do not exist there."""

    def _write(self, text: str) -> Path:
        f = tempfile.NamedTemporaryFile(
            "w", suffix=".baseline", delete=False, encoding="utf-8")
        f.write(text)
        f.close()
        p = Path(f.name)
        self.addCleanup(lambda: p.unlink(missing_ok=True))
        return p

    def test_missing_file_raises(self):
        missing = fo.REPO / "no-such-baseline-xyz.baseline"
        self.assertFalse(missing.exists())
        with self.assertRaises(fo.BaselineError):
            fo.read_baseline(missing)

    def test_only_comments_zero_integer_lines_raises(self):
        p = self._write("# just a comment\n# another\n\n")
        with self.assertRaises(fo.BaselineError):
            fo.read_baseline(p)

    def test_two_integer_lines_raises(self):
        p = self._write("# header\n33\n34\n")
        with self.assertRaises(fo.BaselineError):
            fo.read_baseline(p)

    def test_non_integer_content_raises(self):
        p = self._write("# header\nthirty-four\n")
        with self.assertRaises(fo.BaselineError):
            fo.read_baseline(p)

    def test_negative_integer_raises(self):
        p = self._write("# header\n-5\n")
        with self.assertRaises(fo.BaselineError):
            fo.read_baseline(p)

    def test_missing_file_does_not_return_zero(self):
        # Belt-and-suspenders: the failure MUST be an exception, not a 0 sentinel.
        missing = fo.REPO / "no-such-baseline-abc.baseline"
        try:
            val = fo.read_baseline(missing)
        except fo.BaselineError:
            return
        self.fail(f"missing baseline returned {val!r} instead of failing closed")

    def test_happy_path_comments_plus_one_integer(self):
        p = self._write("# fail-open-guard ratchet baseline\n# more prose\n34\n")
        self.assertEqual(fo.read_baseline(p), 34)


class AllCratesCountFailClosed(unittest.TestCase):
    """`all_crates_count` must fail closed (raise) when discovery is empty —
    an empty target list is broken discovery, not a workspace with zero
    swallows. RED on origin/main: the function does not exist there."""

    def test_empty_discovery_raises_not_zero(self):
        saved = fo.iter_target_files
        try:
            fo.iter_target_files = lambda all_crates: []
            with self.assertRaises(fo.BaselineError):
                fo.all_crates_count()
        finally:
            fo.iter_target_files = saved

    def test_empty_discovery_never_returns_zero(self):
        saved = fo.iter_target_files
        try:
            fo.iter_target_files = lambda all_crates: []
            try:
                val = fo.all_crates_count()
            except fo.BaselineError:
                return
            self.fail(f"empty discovery returned {val!r} instead of failing closed")
        finally:
            fo.iter_target_files = saved


class RatchetMainEndToEnd(unittest.TestCase):
    """End-to-end `main(["--ratchet"])` exit-code wiring: 0 hold / 1 drift /
    2 undetermined (fail-closed), mirroring check-test-weakening.py.

    DISCRIMINATING (RED on origin/main): the pre-ratchet scanner has no
    `--ratchet` branch, so `main(["--ratchet"])` falls through to a clean
    gate-surface scan and returns 0 for EVERY case below — it cannot block a
    regression, cannot demand an improvement be locked in, and cannot fail
    closed on an undetermined baseline. These tests go red on that old code
    and green only once the ratchet exists."""

    # setUp is deliberately tolerant of a MISSING attribute (getattr default) so
    # that on the pre-ratchet scanner — where all_crates_count / read_baseline do
    # not exist — the test still REACHES main() and observes its real behavior
    # (falling through to a clean gate-surface scan → exit 0), rather than
    # erroring out in setUp. That is what makes these behaviorally discriminating.
    _MISSING = object()

    def setUp(self):
        self._names = ("all_crates_count", "read_baseline", "iter_target_files")
        self._saved = {n: getattr(fo, n, self._MISSING) for n in self._names}

    def tearDown(self):
        for n, v in self._saved.items():
            if v is self._MISSING:
                if hasattr(fo, n):
                    delattr(fo, n)
            else:
                setattr(fo, n, v)

    def _pin(self, count: int, baseline: int):
        fo.all_crates_count = lambda: count
        fo.read_baseline = lambda path=None: baseline

    def test_hold_count_equals_baseline_exit0(self):
        self._pin(34, 34)
        self.assertEqual(fo.main(["check-fail-open.py", "--ratchet"]), 0)

    def test_regression_count_above_baseline_exit1(self):
        self._pin(35, 34)
        # Old code returns 0 here (no --ratchet handling) → RED.
        self.assertEqual(fo.main(["check-fail-open.py", "--ratchet"]), 1)

    def test_improvement_count_below_baseline_exit1(self):
        self._pin(30, 34)
        self.assertEqual(fo.main(["check-fail-open.py", "--ratchet"]), 1)

    def test_unreadable_baseline_is_undetermined_exit2(self):
        def boom(path=None):
            raise fo.BaselineError("simulated unreadable baseline")
        fo.all_crates_count = lambda: 34
        fo.read_baseline = boom
        # Old code returns 0 (fell through to a clean scan) → RED.
        self.assertEqual(fo.main(["check-fail-open.py", "--ratchet"]), 2)

    def test_empty_discovery_is_undetermined_exit2(self):
        # Drive the REAL all_crates_count fail-closed through main by breaking
        # discovery, rather than stubbing the count function.
        fo.iter_target_files = lambda all_crates: []
        fo.read_baseline = lambda path=None: 34
        self.assertEqual(fo.main(["check-fail-open.py", "--ratchet"]), 2)

    def test_regression_prints_the_swallow_locations(self):
        # A regression must surface WHICH swallows exist. Plant a fake finding
        # via a stubbed scan surface so the block is findable.
        self._pin(1, 0)
        import io
        import contextlib
        buf = io.StringIO()
        with contextlib.redirect_stderr(buf), contextlib.redirect_stdout(buf):
            rc = fo.main(["check-fail-open.py", "--ratchet"])
        self.assertEqual(rc, 1)
        self.assertIn("ROSE", buf.getvalue())


class UpdateBaselineRepins(unittest.TestCase):
    """`main(["--update-baseline"])` re-pins the baseline file to the live count
    and exits 0, and the written file round-trips through read_baseline.

    RED on origin/main: no `--update-baseline` handling there (it would fall
    through to a scan and never write the baseline)."""

    def setUp(self):
        self._saved_count = fo.all_crates_count
        self._saved_baseline_file = fo.BASELINE_FILE
        self._tmp = Path(tempfile.mkdtemp()) / "check-fail-open.baseline"

    def tearDown(self):
        fo.all_crates_count = self._saved_count
        fo.BASELINE_FILE = self._saved_baseline_file
        if self._tmp.exists():
            self._tmp.unlink()

    def test_update_baseline_writes_current_count_and_roundtrips(self):
        fo.all_crates_count = lambda: 7
        fo.BASELINE_FILE = self._tmp  # _write_baseline reads this global at call time
        rc = fo.main(["check-fail-open.py", "--update-baseline"])
        self.assertEqual(rc, 0)
        self.assertTrue(self._tmp.exists(), "baseline file must be written")
        # The pinned value round-trips through the reader.
        self.assertEqual(fo.read_baseline(self._tmp), 7)

    def test_update_baseline_fails_closed_when_count_undetermined(self):
        def boom():
            raise fo.BaselineError("broken discovery")
        fo.all_crates_count = boom
        fo.BASELINE_FILE = self._tmp
        rc = fo.main(["check-fail-open.py", "--update-baseline"])
        self.assertEqual(rc, 2, "cannot pin an undetermined count — must fail closed")
        self.assertFalse(self._tmp.exists(), "must not write a baseline it cannot trust")


if __name__ == "__main__":
    unittest.main(verbosity=2)


class EmptyCollectionFallbackClass(unittest.TestCase):
    """The class backlog b0cacd15 filed: 'an error is discarded and an EMPTY
    collection is returned', which downstream reads as 'nothing to inspect →
    clean'. CLAUDE.md §3 names it explicitly ('エラー時に空の集合を返さない').

    Before this class was added, `check-fail-open.baseline` sat at 0 while the
    workspace held dozens of instances — the pinned 0 meant 'zero of the two
    read_dir shapes', not 'zero fail-open'. These tests are the RED that proves
    the scanner did not see the class at all.
    """

    def test_err_arm_substitutes_an_empty_vec(self):
        # `crates/overwatch/src/store.rs` shape: any read failure (permission,
        # IO) becomes the same empty history as a legitimately absent file.
        src = [
            "pub fn read_events(cwd: &Path) -> Result<Vec<LifecycleEvent>> {",
            "    match std::fs::read_to_string(&path) {",
            "        Ok(txt) => Ok(parse(txt)),",
            "        Err(_) => Ok(Vec::new()),",
            "    }",
            "}",
        ]
        self.assertIn("err-arm-empty-fallback", names(fo.scan_rust(src)))

    def test_err_arm_substitutes_a_default(self):
        src = [
            "    match load(&path) {",
            "        Ok(r) => r,",
            "        Err(_) => Registry::default(),",
            "    }",
        ]
        self.assertIn("err-arm-empty-fallback", names(fo.scan_rust(src)))

    def test_unwrap_or_default_after_a_read(self):
        # `crates/harness-status/src/path_shadow.rs:100-107` shape, named
        # verbatim in b0cacd15.
        src = [
            "fn list_binary_names(dir: &Path) -> Vec<String> {",
            "    std::fs::read_dir(dir)",
            "        .map(|rd| collect(rd))",
            "        .unwrap_or_default()",
            "}",
        ]
        self.assertIn("read-unwrap-or-empty", names(fo.scan_rust(src)))

    def test_unwrap_or_false_after_a_read(self):
        # `crates/harness-status/src/plugins.rs:106-108` shape: an unreadable
        # directory is mapped to the same `false` as a genuinely empty one.
        src = [
            "fn dir_nonempty(dir: &Path) -> bool {",
            "    std::fs::read_dir(dir).map(|mut r| r.next().is_some()).unwrap_or(false)",
            "}",
        ]
        self.assertIn("read-unwrap-or-empty", names(fo.scan_rust(src)))

    def test_if_let_ok_drops_unparseable_records_in_a_loop(self):
        # Form B: a malformed ledger line is silently skipped, so a corrupt
        # ledger reads as a SHORTER history rather than an unreadable one.
        src = [
            "    let mut events = Vec::new();",
            "    for line in txt.lines() {",
            "        if let Ok(event) = serde_json::from_str::<LifecycleEvent>(line) {",
            "            events.push(event);",
            "        }",
            "    }",
        ]
        self.assertIn("loop-parse-drop", names(fo.scan_rust(src)))


class EmptyCollectionFallbackFalsePositives(unittest.TestCase):
    """Anti-vacuity controls. A detector that fires on the FIXED forms too would
    have a meaningless count, and would push authors to disable it."""

    def test_propagating_err_arm_is_not_flagged(self):
        src = [
            "    match read(&path) {",
            "        Ok(v) => Ok(v),",
            "        Err(e) => Err(e),",
            "    }",
        ]
        self.assertEqual(fo.scan_rust(src), [])

    def test_tri_stated_err_arm_is_not_flagged(self):
        # The fixed form this repo migrates TO: the error becomes an explicit
        # third value, not an empty one.
        src = [
            "    match read(&path) {",
            "        Ok(v) => Determination::Known(v),",
            "        Err(e) => Determination::undetermined(e.to_string()),",
            "    }",
        ]
        self.assertEqual(fo.scan_rust(src), [])

    def test_unwrap_or_default_without_a_read_is_not_flagged(self):
        # Receiver-aware: `unwrap_or_default` on a parsed number, a config
        # lookup, etc. is not an IO swallow.
        src = ["let n: u32 = s.parse().unwrap_or_default();"]
        self.assertEqual(fo.scan_rust(src), [])

    def test_if_let_ok_outside_a_loop_is_not_flagged(self):
        src = [
            "    if let Ok(cfg) = toml::from_str::<FileConfig>(&text) {",
            "        apply(cfg);",
            "    }",
        ]
        self.assertEqual(fo.scan_rust(src), [])

    def test_err_arm_in_a_comment_is_not_flagged(self):
        src = ["    // used to be `Err(_) => Ok(Vec::new())`, which reported"]
        self.assertEqual(fo.scan_rust(src), [])


class NewClassIsAdvisoryOnly(unittest.TestCase):
    """The 2026-08-06 landing decision: the new class enters the ADVISORY /
    `--ratchet` surface only, NOT the merge-blocking gate-surface verdict.

    Why this is enforced by a test rather than a convention: if the new patterns
    entered the blocking path they would fire on ~9 pre-existing overwatch sites
    at once, and the only way to get a commit through would be to fill ALLOWLIST
    with grandfather entries — turning the reviewed-exception hatch into the
    default escape route (CLAUDE.md §5). The burn-down pressure is the baseline
    diff instead, and no allowlist entry is created for this class.
    """

    def test_new_pattern_names_are_declared_advisory_only(self):
        self.assertEqual(
            fo.ADVISORY_ONLY_PATTERNS,
            frozenset({"err-arm-empty-fallback", "read-unwrap-or-empty",
                       "loop-parse-drop"}),
        )

    def test_blocking_scan_drops_the_advisory_class(self):
        hits = [
            (1, "Err(_) => Ok(Vec::new()),", "err-arm-empty-fallback"),
            (2, "let Ok(e) = read_dir(d) else { return };", "readdir-let-else-swallow"),
        ]
        self.assertEqual(
            fo.blocking_hits(hits),
            [(2, "let Ok(e) = read_dir(d) else { return };",
              "readdir-let-else-swallow")],
        )

    def test_advisory_class_still_counts_toward_the_ratchet(self):
        # `blocking_hits` is the ONLY filter; the ratchet counts raw hits, so a
        # new instance of the advisory class still moves the pinned number.
        hits = [(1, "Err(_) => Ok(Vec::new()),", "err-arm-empty-fallback")]
        self.assertEqual(len(hits), 1)
        self.assertEqual(fo.blocking_hits(hits), [])
