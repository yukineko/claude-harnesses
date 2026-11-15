#!/usr/bin/env python3
"""Tests for .githooks/pre-push blocking check 5 (fail-open mutation over the
range's gate crates; issue #1584).

Run: `python3 scripts/test_prepush_fail_open_mutation.py` (stdlib `unittest`).
Reuses `HookHarness` from scripts/test_prepush_hook.py. The real
scripts/check-fail-open-mutation.py is replaced by a committed stub that logs
its argv to $MUT_LOG (outside the repo) and exits $MUT_EXIT, so only the hook's
wiring is measured, not the mutation harness (that costs minutes).

Contract pinned here:
  (a) a range touching only crates/blastguard runs the checker for blastguard
      only, as `--crate blastguard --keep-going`;
  (b) a range touching no gate crate never runs it;
  (c) a checker that exits 1 blocks the push;
  (d) a recorded pass for the same tree is not rerun; a green run records.

What this does NOT prove: that the real checker catches anything, the cost of
running it, or the all-7 fallbacks (range uncomputable / script changed).
The specguard gate-check that also fires on gate-crate ranges is satisfied with
a stub `target/debug/specguard` that exits 0; cargo is a stub as everywhere.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_prepush_hook import HookTestCase, _CARGO_STUB  # noqa: E402

MUT_STUB = """import os, sys
with open(os.environ["MUT_LOG"], "a") as f:
    f.write(" ".join(sys.argv[1:]) + "\\n")
sys.exit(int(os.environ.get("MUT_EXIT", "0")))
"""

SPECGUARD_STUB = "#!/bin/sh\nexit 0\n"

GATE_PKG = '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n'


def _out(proc):
    return "--- exit %d ---\n--- stdout ---\n%s\n--- stderr ---\n%s" % (
        proc.returncode, proc.stdout, proc.stderr)


class MutationCheck(HookTestCase):
    def mut_harness(self, mut_exit=0):
        h = self.harness(crate=True)
        h.mut_log = h.logdir / "mutation.log"
        h.env["MUT_LOG"] = str(h.mut_log)
        h.env["MUT_EXIT"] = str(mut_exit)
        # cargo metadata must know the gate crates, or the hook's cargo-check
        # step blocks first ("maps to 0 workspace packages").
        pkgs = [{"name": n, "version": "0.1.0", "id": n + " 0.1.0",
                 "manifest_path": str(h.root / "crates" / n / "Cargo.toml")}
                for n in ("foo", "blastguard", "propguard")]
        metadata = json.dumps({
            "packages": pkgs, "workspace_members": [p["id"] for p in pkgs],
            "target_directory": str(h.root / "target"),
            "workspace_root": str(h.root), "version": 1})
        cargo = h.bindir / "cargo"
        cargo.write_text(_CARGO_STUB.format(code=0, metadata=metadata))
        h.commit_files({"scripts/check-fail-open-mutation.py": MUT_STUB}, "add stub")
        # specguard gate-check (fires on gate-crate ranges) needs a built binary
        # at <target_directory>/debug/specguard; target/ is untracked + outside
        # the crate dirs the clean-checkout test inspects.
        sg = h.root / "target" / "debug" / "specguard"
        sg.parent.mkdir(parents=True)
        sg.write_text(SPECGUARD_STUB)
        sg.chmod(0o755)
        h.base = h.local_sha
        return h

    def push(self, h, files):
        h.commit_files(files, "touch")
        return h.run(stdin="refs/heads/main %s refs/heads/main %s\n" % (h.local_sha, h.base))

    @staticmethod
    def gate(name):
        return {"crates/%s/Cargo.toml" % name: GATE_PKG % name,
                "crates/%s/src/lib.rs" % name: "pub fn g() {}\n"}

    def calls(self, h):
        if not h.mut_log.is_file():
            return []
        return h.mut_log.read_text().splitlines()

    # (a)
    def test_blastguard_only_range_runs_blastguard_only(self):
        h = self.mut_harness()
        proc = self.push(h, self.gate("blastguard"))
        self.assertAllowed(proc, _out(proc))
        self.assertEqual(self.calls(h), ["--crate blastguard --keep-going"], _out(proc))

    def test_control_two_gate_crates_run_both(self):
        h = self.mut_harness()
        files = self.gate("blastguard")
        files.update(self.gate("propguard"))
        proc = self.push(h, files)
        self.assertAllowed(proc, _out(proc))
        self.assertEqual(sorted(self.calls(h)),
                         ["--crate blastguard --keep-going", "--crate propguard --keep-going"],
                         _out(proc))

    # (b)
    def test_no_gate_crate_in_range_never_runs_it(self):
        for label, files in (
            ("non-gate crate", {"crates/foo/src/lib.rs": "pub fn g() {}\n"}),
            ("docs only", {"docs/notes.md": "words\n"}),
        ):
            with self.subTest(label):
                h = self.mut_harness()
                proc = self.push(h, files)
                self.assertAllowed(proc, _out(proc))
                self.assertEqual(self.calls(h), [], _out(proc))
                self.assertNotIn("fail-open mutation", proc.stderr)

    # (c)
    def test_checker_exit_1_blocks(self):
        h = self.mut_harness(mut_exit=1)
        proc = self.push(h, self.gate("blastguard"))
        self.assertBlocked(proc)
        self.assertEqual(self.calls(h), ["--crate blastguard --keep-going"], _out(proc))
        self.assertIn("BLOCKED", proc.stderr)
        self.assertIn("fail-open mutation check for blastguard", proc.stderr)
        self.assertFalse(h.record_path(h.tree_of(h.local_sha),
                                       "fail-open-mutation:blastguard").exists())

    def test_control_checker_exit_0_allows(self):
        h = self.mut_harness(mut_exit=0)
        proc = self.push(h, self.gate("blastguard"))
        self.assertAllowed(proc, _out(proc))
        self.assertEqual(len(self.calls(h)), 1, _out(proc))

    # (d)
    def test_recorded_pass_for_same_tree_is_not_rerun(self):
        h = self.mut_harness(mut_exit=1)  # would block if it ran
        h.commit_files(self.gate("blastguard"), "touch")
        tree = h.tree_of(h.local_sha)
        h.write_record(tree, "fail-open-mutation:blastguard")
        proc = h.run(stdin="refs/heads/main %s refs/heads/main %s\n" % (h.local_sha, h.base))
        self.assertAllowed(proc, _out(proc))
        self.assertEqual(self.calls(h), [], _out(proc))
        self.assertIn("pre-push: fail-open-mutation:blastguard already passed for tree %s"
                      % tree[:12], proc.stderr)

    def test_green_run_records_then_same_push_skips(self):
        h = self.mut_harness()
        first = self.push(h, self.gate("blastguard"))
        self.assertAllowed(first, _out(first))
        tree = h.tree_of(h.local_sha)
        self.assertTrue(h.record_path(tree, "fail-open-mutation:blastguard").is_file(),
                        _out(first))
        h.mut_log.unlink()
        second = h.run(stdin="refs/heads/main %s refs/heads/main %s\n" % (h.local_sha, h.base))
        self.assertAllowed(second, _out(second))
        self.assertEqual(self.calls(h), [], _out(second))

    def test_record_for_a_different_tree_still_runs(self):
        h = self.mut_harness()
        h.commit_files(self.gate("blastguard"), "touch")
        h.write_record("4b825dc642cb6eb9a060e54bf8d69288fbee4904",
                       "fail-open-mutation:blastguard")
        proc = h.run(stdin="refs/heads/main %s refs/heads/main %s\n" % (h.local_sha, h.base))
        self.assertAllowed(proc, _out(proc))
        self.assertEqual(len(self.calls(h)), 1, _out(proc))


if __name__ == "__main__":
    unittest.main(verbosity=2)
