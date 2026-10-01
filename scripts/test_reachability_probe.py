#!/usr/bin/env python3
"""Tests for scripts/reachability-probe.py (written by an independent author;
the script is implemented separately against THIS contract).

CLI contract (pinned):

  python3 scripts/reachability-probe.py \
      --file <path> --anchor <exact literal old string> \
      [--replacement <text>]      # default: panic!("reachability-probe")
      --out <result.json> \
      [--test-cmd <shell cmd>]    # default: cargo test -p <crate>
      [--build-cmd <shell cmd>]   # default: cargo test -p <crate> --no-run
      [--crate <name>]

Flow: snapshot target bytes -> baseline test cmd must exit 0 else exit 2, no
result -> anchor must occur EXACTLY once else exit 2, no result -> replace
anchor with replacement -> build cmd must exit 0 else exit 2, no result
(a compile failure is never a reproduction) -> run test cmd -> restore in a
`finally`, verify byte-identical. If restore cannot be verified, exit non-zero
and write no result file.

Result JSON: {"result": "reproduced" | "not_reproduced", ...extra keys}.
  reproduced     <=> test cmd exits non-zero after mutation
  not_reproduced <=> test cmd exits 0 after mutation
Extra keys MUST include "note" (a string stating not_reproduced does not
establish unreachability) and "panic_marker_seen" (bool: the marker
"reachability-probe" appeared in the test output).
  `note` MUST contain the canonical phrase "does not establish unreachability".
Exit codes: 0 = a result was written; 2 = undetermined (no result file).
Commands run via `sh -c <cmd>` as a DIRECT child of the probe (so `$PPID` in a
cmd is the probe's pid), with the caller's cwd. Any non-zero exit of the test
cmd after mutation, including 127 (command not found), counts as "reproduced".
The probe MUST restore the target on SIGTERM and SIGINT too (not only on the
normal path); a probe killed that way writes no result file and exits non-zero.
The result must be accepted by overwatch's parse_probe
(crates/overwatch/src/review_finding.rs): a JSON object whose string `result`
is exactly one of the two tokens; extra keys are allowed.
"""
import hashlib
import json
import os
import re
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCRIPT = HERE / "reachability-probe.py"
REPO_ROOT = HERE.parent
MARKER = "reachability-probe"
ORIGINAL = "fn a() {}\nif cond { BRANCH_X }\nfn c() {}\n"
ANCHOR = "BRANCH_X"

# Test cmd: red iff the (mutated) target contains the marker; prints it like a panic.
REACH_CMD = "if grep -q reachability-probe src.rs; then echo \"panicked at reachability-probe\"; exit 1; fi; exit 0"


def sha(p):
    return hashlib.sha256(Path(p).read_bytes()).hexdigest()


def parse_probe(text):
    """Python mirror of overwatch parse_probe (review_finding.rs)."""
    try:
        v = json.loads(text)
    except Exception:
        return None
    if not isinstance(v, dict):
        return None
    r = v.get("result")
    return r if r in ("not_reproduced", "reproduced") else None


class Base(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory()
        self.dir = Path(self._td.name)
        self.target = self.dir / "src.rs"
        self.target.write_text(ORIGINAL, encoding="utf-8")
        self.out = self.dir / "result.json"
        self.before = sha(self.target)

    def tearDown(self):
        os.chmod(self.dir, 0o755)
        if self.target.exists():
            os.chmod(self.target, 0o644)
        self._td.cleanup()

    def run_probe(self, test_cmd, build_cmd="true", anchor=ANCHOR, extra=()):
        # Python's own exit 2 for a missing script must not satisfy "undetermined".
        self.assertTrue(SCRIPT.is_file(), f"{SCRIPT} does not exist")
        argv = [
            sys.executable, str(SCRIPT),
            "--file", str(self.target), "--anchor", anchor,
            "--out", str(self.out),
            "--test-cmd", test_cmd, "--build-cmd", build_cmd,
            *extra,
        ]
        return subprocess.run(argv, cwd=str(self.dir), capture_output=True, text=True, timeout=120)

    def assert_restored(self):
        self.assertEqual(sha(self.target), self.before, "target not byte-identical")

    def assert_undetermined(self, r):
        self.assertEqual(r.returncode, 2, r.stdout + r.stderr)
        self.assertFalse(self.out.exists(), "result file must not exist")


class Outcomes(Base):
    def test_reached_branch_is_reproduced_and_parses(self):
        r = self.run_probe(REACH_CMD)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        body = self.out.read_text(encoding="utf-8")
        self.assertEqual(parse_probe(body), "reproduced")
        d = json.loads(body)
        self.assertEqual(d["result"], "reproduced")
        self.assertIs(d["panic_marker_seen"], True)
        self.assert_restored()

    def test_unreached_is_not_reproduced_with_note(self):
        r = self.run_probe("exit 0")
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        body = self.out.read_text(encoding="utf-8")
        self.assertEqual(parse_probe(body), "not_reproduced")
        d = json.loads(body)
        self.assertIsInstance(d.get("note"), str)
        self.assertIn("does not establish unreachability", d["note"].lower())
        self.assertIs(d["panic_marker_seen"], False)
        self.assert_restored()

    def test_failing_for_other_reason_has_marker_false(self):
        # Non-zero after mutation without the marker: reproduced, marker not seen.
        cmd = "if grep -q reachability-probe src.rs; then echo unrelated failure; exit 1; fi; exit 0"
        r = self.run_probe(cmd)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        d = json.loads(self.out.read_text(encoding="utf-8"))
        self.assertEqual(d["result"], "reproduced")
        self.assertIs(d["panic_marker_seen"], False)

    def test_custom_replacement_is_applied(self):
        cmd = "if grep -q CUSTOM_REPL src.rs; then exit 1; fi; exit 0"
        r = self.run_probe(cmd, extra=("--replacement", "CUSTOM_REPL"))
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertEqual(json.loads(self.out.read_text())["result"], "reproduced")
        self.assert_restored()


class Undetermined(Base):
    def test_baseline_red_exits_2(self):
        self.assert_undetermined(self.run_probe("exit 1"))
        self.assert_restored()

    def test_build_failure_exits_2(self):
        b = "if grep -q reachability-probe src.rs; then exit 1; fi; exit 0"
        self.assert_undetermined(self.run_probe(REACH_CMD, build_cmd=b))
        self.assert_restored()

    def test_missing_build_binary_exits_2_and_restores(self):
        self.assert_undetermined(self.run_probe("exit 0", build_cmd="/nonexistent/binary-xyz"))
        self.assert_restored()

    def test_anchor_zero_matches(self):
        self.assert_undetermined(self.run_probe("exit 0", anchor="NOT_PRESENT_ANYWHERE"))
        self.assert_restored()

    def test_anchor_multiple_matches(self):
        self.target.write_text(ORIGINAL + "BRANCH_X again\n", encoding="utf-8")
        self.before = sha(self.target)
        self.assert_undetermined(self.run_probe("exit 0"))
        self.assert_restored()


class Restore(Base):
    def test_test_cmd_corrupting_file_is_still_restored(self):
        cmd = "if grep -q reachability-probe src.rs; then echo garbage > src.rs; exit 1; fi; exit 0"
        r = self.run_probe(cmd)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assert_restored()

    def test_missing_test_binary_mid_run_restores(self):
        # Baseline must pass, so switch to a missing binary only once mutated.
        cmd = "if grep -q reachability-probe src.rs; then /nonexistent/binary-xyz; fi; exit 0"
        r = self.run_probe(cmd)
        # Contract: exit 127 is a non-zero exit after mutation => reproduced, no marker.
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)
        d = json.loads(self.out.read_text(encoding="utf-8"))
        self.assertEqual(d["result"], "reproduced")
        self.assertIs(d["panic_marker_seen"], False)
        self.assert_restored()

    def _signal_case(self, sig):
        cmd = f"if grep -q reachability-probe src.rs; then kill -{sig} $PPID; sleep 5; fi; exit 0"
        r = self.run_probe(cmd)
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertFalse(self.out.exists(), "killed probe must write no result")
        self.assert_restored()

    def test_sigterm_mid_run_restores(self):
        self._signal_case("TERM")

    def test_sigint_mid_run_restores(self):
        self._signal_case("INT")

    def test_unverifiable_restore_is_nonzero_without_result(self):
        # Target replaced by a directory once mutated: restore cannot succeed.
        # --out is in the (writable) tempdir, so a result write WOULD succeed.
        cmd = "if grep -q reachability-probe src.rs; then rm -f src.rs; mkdir src.rs; exit 1; fi; exit 0"
        r = self.run_probe(cmd)
        self.assertNotEqual(r.returncode, 0, r.stdout + r.stderr)
        self.assertFalse(self.out.exists(), "no result file when restore unverified")


class ParserContract(unittest.TestCase):
    def test_overwatch_parse_probe_still_has_the_pinned_tokens(self):
        src = (REPO_ROOT / "crates/overwatch/src/review_finding.rs").read_text(encoding="utf-8")
        self.assertIn('Some("not_reproduced")', src)
        self.assertIn('Some("reproduced")', src)
        self.assertRegex(src, re.compile(r"get\(\"result\"\)"))

    def test_mirror_accepts_extra_keys_rejects_unknown(self):
        self.assertEqual(parse_probe('{"result":"reproduced","note":"x","panic_marker_seen":true}'), "reproduced")
        self.assertIsNone(parse_probe('{"result":"probably_fine"}'))


if __name__ == "__main__":
    unittest.main()
