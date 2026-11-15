#!/usr/bin/env python3
"""Tests for .githooks/pre-push's range scoping (user ruling 2026-11-08: hooks
check only the change's range).

Run: `python3 scripts/test_prepush_range_scope.py` (stdlib `unittest` only).
Reuses `HookHarness` from scripts/test_prepush_hook.py; set
PREPUSH_HOOK_UNDER_TEST / TEST_PASS_RECORD_UNDER_TEST to point it at other
copies.

Contract pinned here (SPEC section C):
  * the hook runs no `git worktree` command and leaves no extra worktree;
  * cargo runs with cwd = the repo root, CARGO_TARGET_DIR not overridden, and
    `cargo check --all-targets --quiet -p <pkg>` (package from `cargo
    metadata`, matched by manifest_path) or `--workspace`;
  * a range with no cargo input runs no `cargo check` (and no compile-fail);
  * a valid pass record for (tree of S, test id) skips the check with
    `pre-push: <ID> already passed for tree <12 hex> — not rerun`; a record for
    another tree, or a corrupt one, does not;
  * with no record, a pushed S that is not HEAD, or a dirty / untracked-crate
    checkout, BLOCKS;
  * a green in-place run writes the record, so the same push again skips;
  * the bypass ledger gets `--push <HEAD> <40 zeros>` on a manual run (no
    stdin, no @{push}), and is not called for a delete-only push.

What this does NOT prove: anything about the specguard gate-check (no gate
crate is touched here), the trybuild check beyond whether its stub ran, or the
real cargo's behaviour (cargo is a stub).
"""

from __future__ import annotations

import json
import re
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_prepush_hook import (  # noqa: E402
    CRATE_DIR,
    CRATE_PKG,
    HookTestCase,
    _ZERO_SHA,
    _which,
)

CHECK_ID = "cargo-check:" + CRATE_DIR


def _out(proc):
    return "--- exit %d ---\n--- stdout ---\n%s\n--- stderr ---\n%s" % (
        proc.returncode, proc.stdout, proc.stderr)


class RangeTestCase(HookTestCase):
    def assertNoCargoCheck(self, h, proc):
        self.assertEqual(h.cargo_checks(), [],
                         "cargo check must not run\n" + _out(proc))

    def assertCargoCheckRan(self, h, proc):
        self.assertTrue(h.cargo_checks(), "cargo check must run\n" + _out(proc))


class NoWorktree(RangeTestCase):
    def test_pre_push_runs_no_git_worktree_command(self):
        h = self.harness(crate=True, git_log=True)
        proc = h.run()
        self.assertAllowed(proc)
        # anti-vacuity: the wrapper really logged the hook's git calls, and the
        # type check (where a worktree used to be made) really ran.
        self.assertTrue(h.git_calls(), "the git wrapper logged nothing\n" + _out(proc))
        self.assertCargoCheckRan(h, proc)
        offending = [c for c in h.git_calls() if "worktree" in c]
        self.assertEqual(offending, [], "the hook ran `git worktree`:\n%r" % offending)
        listed = subprocess.run(
            [_which("git"), "-C", str(h.root), "worktree", "list", "--porcelain"],
            capture_output=True, text=True, check=True,
        ).stdout
        self.assertEqual(
            [ln for ln in listed.splitlines() if ln.startswith("worktree ")],
            ["worktree " + str(h.root)],
        )


class CargoInvocation(RangeTestCase):
    def test_cargo_check_runs_in_the_repo_root_with_dash_p_package(self):
        h = self.harness(crate=True)
        proc = h.run()
        self.assertAllowed(proc)
        checks = h.cargo_checks()
        self.assertEqual(len(checks), 1, "exactly one cargo check: %r\n%s" % (checks, _out(proc)))
        cwd, target_dir, args = checks[0]
        self.assertEqual(Path(cwd).resolve(), h.root, "cargo must run in the repo root")
        self.assertEqual(target_dir, "<unset>", "CARGO_TARGET_DIR must not be overridden")
        self.assertIn("--all-targets", args)
        self.assertNotIn("--workspace", args)
        pairs = [(args[i], args[i + 1]) for i in range(len(args) - 1)]
        self.assertIn(("-p", CRATE_PKG), pairs,
                      "the package name comes from cargo metadata, not the dir: %r" % args)
        metadata = [c for c in h.cargo_calls() if c[2][:1] == ["metadata"]]
        self.assertTrue(metadata, "package names must come from `cargo metadata`")
        self.assertIn("--no-deps", metadata[0][2])

    def test_root_cargo_toml_widens_to_workspace(self):
        h = self.harness(crate=True)
        base = h.local_sha
        h.commit_files({"Cargo.toml": "[workspace]\nmembers = [\"crates/foo\"]\n"})
        proc = h.run(stdin="refs/heads/main %s refs/heads/main %s\n" % (h.local_sha, base))
        self.assertAllowed(proc)
        checks = h.cargo_checks()
        self.assertEqual(len(checks), 1, _out(proc))
        self.assertIn("--workspace", checks[0][2])
        self.assertNotIn("-p", checks[0][2])

    def test_crate_dir_without_cargo_toml_widens_to_workspace(self):
        h = self.harness(crate=True)
        base = h.local_sha
        h.commit_files({"crates/bar/notes.txt": "no manifest here\n"})
        proc = h.run(stdin="refs/heads/main %s refs/heads/main %s\n" % (h.local_sha, base))
        self.assertAllowed(proc)
        checks = h.cargo_checks()
        self.assertEqual(len(checks), 1, _out(proc))
        self.assertIn("--workspace", checks[0][2])


class NonCrateRange(RangeTestCase):
    def test_docs_only_push_runs_no_cargo_check(self):
        h = self.harness(crate=True)
        base = h.local_sha
        h.commit_files({"docs/notes.md": "words\n"})
        proc = h.run(stdin="refs/heads/main %s refs/heads/main %s\n" % (h.local_sha, base))
        self.assertAllowed(proc)
        self.assertNoCargoCheck(h, proc)
        self.assertFalse(h.cf_log.exists(),
                         "a docs-only range has no compile-fail input\n" + _out(proc))

    def test_control_crate_change_over_the_same_base_runs_cargo_check(self):
        h = self.harness(crate=True)
        base = h.local_sha
        h.commit_files({CRATE_DIR + "/src/lib.rs": "pub fn g() {}\n"})
        proc = h.run(stdin="refs/heads/main %s refs/heads/main %s\n" % (h.local_sha, base))
        self.assertAllowed(proc)
        self.assertCargoCheckRan(h, proc)
        self.assertTrue(h.cf_log.exists(), "a .rs change needs compile-fail\n" + _out(proc))


class RecordSkip(RangeTestCase):
    def test_recorded_pass_skips_cargo_check(self):
        h = self.harness(crate=True)
        tree = h.tree_of(h.local_sha)
        h.write_record(tree, CHECK_ID)
        proc = h.run()
        self.assertAllowed(proc)
        self.assertNoCargoCheck(h, proc)
        self.assertIn("already passed", proc.stderr)
        self.assertIn(
            "pre-push: %s already passed for tree %s — not rerun" % (CHECK_ID, tree[:12]),
            proc.stderr,
        )

    def test_record_for_a_different_tree_runs_the_check(self):
        other_tree = "4b825dc642cb6eb9a060e54bf8d69288fbee4904"  # the empty tree
        for where in ("filed under the other tree", "filed under S but naming the other tree"):
            with self.subTest(where):
                h = self.harness(crate=True)
                tree = h.tree_of(h.local_sha)
                self.assertNotEqual(other_tree, tree)
                if where == "filed under the other tree":
                    h.write_record(other_tree, CHECK_ID)
                else:
                    h.write_record(tree, CHECK_ID, content=json.dumps(
                        {"version": 1, "tree": other_tree, "test": CHECK_ID}))
                proc = h.run()
                self.assertAllowed(proc)
                self.assertCargoCheckRan(h, proc)
                self.assertNotIn("%s already passed" % CHECK_ID, proc.stderr)

    def test_corrupt_record_runs_the_check(self):
        h = self.harness(crate=True)
        h.write_record(h.tree_of(h.local_sha), CHECK_ID, content="{not json")
        proc = h.run()
        self.assertCargoCheckRan(h, proc)
        self.assertNotIn("%s already passed" % CHECK_ID, proc.stderr)

    def test_green_run_records_and_the_same_push_again_skips(self):
        h = self.harness(crate=True)
        tree = h.tree_of(h.local_sha)
        first = h.run()
        self.assertAllowed(first)
        self.assertCargoCheckRan(h, first)
        rec = h.record_path(tree, CHECK_ID)
        self.assertTrue(rec.is_file(), "a green run must record its pass at %s\n%s" % (rec, _out(first)))
        self.assertEqual(json.loads(rec.read_text()),
                         {"version": 1, "tree": tree, "test": CHECK_ID})
        h.cargo_log.unlink()
        second = h.run()
        self.assertAllowed(second)
        self.assertNoCargoCheck(h, second)
        self.assertIn("%s already passed for tree %s" % (CHECK_ID, tree[:12]), second.stderr)

    def test_failed_check_records_nothing(self):
        h = self.harness(crate=True, cargo_exit=1)
        proc = h.run()
        self.assertBlocked(proc)
        self.assertFalse(h.record_path(h.tree_of(h.local_sha), CHECK_ID).exists())


class NotTheCleanCheckout(RangeTestCase):
    BLOCK_RE = r"pre-push: BLOCKED — %s for [0-9a-f]{7,} has no recorded pass and .*is not the clean checkout here"

    def _non_head_push(self, h):
        s = h.local_sha
        h.git("branch", "feat", s)
        h.commit_files({"g.txt": "moved on\n"}, "HEAD moves past S")
        self.assertNotEqual(h.git("rev-parse", "HEAD"), s)
        return s, "refs/heads/feat %s refs/heads/feat %s\n" % (s, _ZERO_SHA)

    def test_pushed_sha_that_is_not_head_blocks_without_a_record(self):
        h = self.harness(crate=True)
        s, stdin = self._non_head_push(h)
        proc = h.run(stdin=stdin)
        self.assertBlocked(proc)
        self.assertNoCargoCheck(h, proc)
        self.assertRegex(proc.stderr, re.compile(self.BLOCK_RE % re.escape(CHECK_ID)))

    def test_control_pushed_sha_that_is_not_head_passes_with_records(self):
        h = self.harness(crate=True)
        s, stdin = self._non_head_push(h)
        tree = h.tree_of(s)
        h.write_record(tree, CHECK_ID)
        h.write_record(tree, "compile-fail")
        proc = h.run(stdin=stdin)
        self.assertAllowed(proc, "every needed check has a recorded pass for S's tree")
        self.assertNoCargoCheck(h, proc)

    def test_dirty_tracked_file_blocks_without_a_record(self):
        h = self.harness(crate=True)
        (h.root / "f.txt").write_text("dirty\n")
        proc = h.run()
        self.assertBlocked(proc)
        self.assertNoCargoCheck(h, proc)
        self.assertIn("is not the clean checkout here", proc.stderr)

    def test_untracked_file_under_crates_blocks_without_a_record(self):
        h = self.harness(crate=True)
        (h.root / CRATE_DIR / "src" / "extra.rs").write_text("pub fn x() {}\n")
        proc = h.run()
        self.assertBlocked(proc)
        self.assertNoCargoCheck(h, proc)
        self.assertIn("is not the clean checkout here", proc.stderr)

    def test_control_untracked_file_outside_cargo_inputs_does_not_block(self):
        h = self.harness(crate=True)
        (h.root / "scratch.txt").write_text("not a cargo input\n")
        proc = h.run()
        self.assertAllowed(proc)
        self.assertCargoCheckRan(h, proc)


class BypassLedgerArgs(RangeTestCase):
    def test_manual_run_passes_head_and_zeros(self):
        h = self.harness(crate=False)
        head = h.git("rev-parse", "HEAD")
        proc = h.run(stdin="")
        calls = h.gate_bypass_calls()
        self.assertTrue(calls, "gate-bypass.py was not called\n" + _out(proc))
        self.assertEqual(calls[0], ["--push", head, _ZERO_SHA], _out(proc))

    def test_delete_only_push_skips_the_ledger(self):
        h = self.harness(crate=False, gate_bypass_exit=1)
        proc = h.run(stdin="(delete) %s refs/heads/gone %s\n" % (_ZERO_SHA, h.local_sha))
        self.assertEqual(h.gate_bypass_calls(), [],
                         "a delete-only push carries nothing\n" + _out(proc))

    def test_control_pushed_line_is_forwarded(self):
        h = self.harness(crate=False)
        proc = h.run()
        self.assertEqual(h.gate_bypass_calls()[0], ["--push", h.local_sha, _ZERO_SHA], _out(proc))


if __name__ == "__main__":
    unittest.main(verbosity=2)
