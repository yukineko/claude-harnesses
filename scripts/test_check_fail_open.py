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
        dirty = []
        for p in fo.iter_target_files(all_crates=False):
            hits = fo.scan_file(p)
            if hits:
                dirty.append((str(p.relative_to(fo.REPO)), hits))
        self.assertEqual(dirty, [], f"gate surface not clean: {dirty}")

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
    def test_allowlist_suppresses_only_its_own_hit(self):
        # test_freshness.rs carries two allowlisted swallows → scan_file drops them.
        p = fo.REPO / "crates/overwatch/src/test_freshness.rs"
        if p.exists():
            self.assertEqual(fo.scan_file(p), [],
                             "allowlisted swallows must be suppressed")
            # …but only because of the allowlist: with it emptied they reappear.
            saved = fo.ALLOWLIST[:]
            try:
                fo.ALLOWLIST.clear()
                self.assertNotEqual(fo.scan_file(p), [],
                                    "without the allowlist the real swallow must show")
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


if __name__ == "__main__":
    unittest.main(verbosity=2)
