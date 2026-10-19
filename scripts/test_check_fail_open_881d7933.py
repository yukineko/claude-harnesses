#!/usr/bin/env python3
"""Regression tests for backlog 881d7933 (check-fail-open.py READDIR_WINDOW).

Bug (measured 2026-09-10 at 8c056f05): the `read-unwrap-or-empty` advisory
looks back READDIR_WINDOW (=6) code lines from an `.unwrap_or…` for an IO call.
crates/harness-status/src/path_shadow.rs `list_binary_names` at 8c056f05 put
`std::fs::read_dir(dir)` on line 100 and `.unwrap_or_default()` on line 107 —
7 lines apart — so `--all` did not report it, while the 2-line-apart
plugins.rs `dir_nonempty` site WAS reported.

Pinned properties:
  1. The verbatim 7-line-distance shape of the historical path_shadow.rs is
     reported (RED on window=6).
  2. The real path_shadow.rs blob at 8c056f05 (read via `git show`) is reported
     by the scanner; and if the site still exists in the working tree, `--all`
     reports it (skipped with a visible reason when the site is gone).
  3. Controls so the fix cannot be "make the window infinite" / "match every
     unwrap_or": the plugins.rs site is still reported by `--all`, and a
     read_dir / unwrap_or pair far apart (40 lines) is NOT reported.

Stdlib-only: `python3 scripts/test_check_fail_open_881d7933.py`.
"""

from __future__ import annotations

import contextlib
import importlib.util
import io
import subprocess
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open", _HERE / "check-fail-open.py"
)
fo = importlib.util.module_from_spec(_SPEC)
# Compiled from the SOURCE TEXT, deliberately not via the spec loader's
# exec_module: SourceFileLoader reuses a __pycache__ .pyc validated only by
# (source mtime at 1 s granularity, size), so a same-second size-preserving
# edit (e.g. a reordering mutant) would run stale bytecode -- a false GREEN /
# false mutation SURVIVOR, never a false red. Backlog 05726f9f; do not
# "simplify" this back. get_source() reads the .py, never the cache;
# dont_inherit keeps this file's __future__ flags off the subject.
exec(  # noqa: S102
    compile(_SPEC.loader.get_source(_SPEC.name), _SPEC.origin, "exec", dont_inherit=True),
    fo.__dict__,
)

PATTERN = "read-unwrap-or-empty"
HIST_REV = "8c056f05"
PATH_SHADOW = "crates/harness-status/src/path_shadow.rs"
PLUGINS = "crates/harness-status/src/plugins.rs"

# Verbatim from `git show 8c056f05:crates/harness-status/src/path_shadow.rs`
# lines 98-108 (read_dir on fixture line 3, unwrap_or_default on line 10).
PATH_SHADOW_SHAPE = [
    "/// Non-recursive list of file names directly inside `dir`.",
    "fn list_binary_names(dir: &Path) -> Vec<String> {",
    "    std::fs::read_dir(dir)",
    "        .map(|it| {",
    "            it.filter_map(|e| e.ok())",
    "                .filter(|e| e.path().is_file())",
    "                .filter_map(|e| e.file_name().into_string().ok())",
    "                .collect()",
    "        })",
    "        .unwrap_or_default()",
    "}",
]


def hits_of(lines, pattern=PATTERN):
    return [(ln, n) for ln, _, n in fo.scan_rust(lines) if n == pattern]


def run_all() -> str:
    buf = io.StringIO()
    with contextlib.redirect_stdout(buf), contextlib.redirect_stderr(buf):
        fo.main(["check-fail-open.py", "--all"])
    return buf.getvalue()


class SevenLineDistanceIsReported(unittest.TestCase):
    def test_fixture_distance_is_seven(self):
        # Guard the fixture itself: if someone edits it, the distance claim breaks.
        rd = next(i for i, l in enumerate(PATH_SHADOW_SHAPE) if "read_dir(" in l)
        uo = next(i for i, l in enumerate(PATH_SHADOW_SHAPE) if ".unwrap_or" in l)
        self.assertEqual(uo - rd, 7)

    def test_path_shadow_shape_is_reported(self):
        self.assertEqual(
            hits_of(PATH_SHADOW_SHAPE), [(10, PATTERN)],
            "read_dir 7 lines above .unwrap_or_default() must be reported "
            f"(READDIR_WINDOW={fo.READDIR_WINDOW})",
        )


class RealPathShadowSite(unittest.TestCase):
    def test_historical_blob_is_reported(self):
        try:
            blob = subprocess.run(
                ["git", "-C", str(fo.REPO), "show", f"{HIST_REV}:{PATH_SHADOW}"],
                capture_output=True, text=True, check=True,
            ).stdout
        except (subprocess.CalledProcessError, FileNotFoundError) as e:
            # Cannot determine → fail, never pass.
            self.fail(f"could not read {HIST_REV}:{PATH_SHADOW}: {e}")
        lines = blob.splitlines()
        self.assertIn("        .unwrap_or_default()", lines[106],
                      "historical blob no longer matches the measured site")
        self.assertIn((107, PATTERN), hits_of(lines),
                      f"{HIST_REV}:{PATH_SHADOW}:107 list_binary_names not reported")

    def test_current_tree_site_reported_by_all_if_present(self):
        src = (fo.REPO / PATH_SHADOW).read_text().splitlines()
        sites = [
            i + 1 for i, l in enumerate(src)
            if ".unwrap_or" in l and any(
                "read_dir(" in src[j] for j in range(max(0, i - 12), i)
            )
        ]
        if not sites:
            self.skipTest(
                f"{PATH_SHADOW} no longer has a read_dir..unwrap_or site "
                "(list_binary_names was rewritten to a match); covered by the "
                "historical-blob test instead"
            )
        out = run_all()
        for ln in sites:
            self.assertIn(f"{PATH_SHADOW}:{ln}: [{PATTERN}]", out)


class Controls(unittest.TestCase):
    def test_plugins_dir_nonempty_still_reported_by_all(self):
        src = (fo.REPO / PLUGINS).read_text().splitlines()
        ln = next(
            (i + 1 for i, l in enumerate(src) if ".unwrap_or(false)" in l), None
        )
        self.assertIsNotNone(ln, f"{PLUGINS} dir_nonempty site not found")
        self.assertIn(
            f"{PLUGINS}:{ln}: [{PATTERN}] (advisory) .unwrap_or(false)", run_all()
        )

    def test_far_apart_readdir_unwrap_or_is_not_reported(self):
        src = (
            ["fn f(dir: &Path) -> bool {",
             "    let rd = std::fs::read_dir(dir);"]
            + ["    let _ = 0;"] * 40
            + ["    let x = opt.unwrap_or(false);", "    x", "}"]
        )
        self.assertEqual(hits_of(src), [],
                         "an unwrap_or 41 lines below read_dir is not correlated")

    def test_unwrap_or_without_io_is_not_reported(self):
        src = ["fn f(o: Option<bool>) -> bool {", "    o.unwrap_or(false)", "}"]
        self.assertEqual(hits_of(src), [])


if __name__ == "__main__":
    unittest.main()
