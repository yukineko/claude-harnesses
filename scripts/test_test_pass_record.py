#!/usr/bin/env python3
"""Tests for scripts/test-pass-record.py, the tree-keyed pass record that lets
.githooks/pre-push skip a check it has already seen pass for the same tree
(user ruling 2026-11-08: hooks check only the change's range).

Run: `python3 scripts/test_test_pass_record.py` (stdlib `unittest` only).
Set TEST_PASS_RECORD_UNDER_TEST to point the suite at another copy.

Contract pinned here:

  check --tree T --test ID
      0 ONLY when a valid record exists for exactly (T, ID). Every other case
      is 1: missing record, non-JSON, wrong shape or version, stored tree or
      test differing from the request, a T that is not 40/64 lowercase hex,
      or a store that cannot be resolved. Every case except a plain missing
      record prints one stderr line with the reason.
  record --tree T --test ID
      T must be full lowercase hex AND `git cat-file -t T` must print `tree`,
      otherwise exit 2 and nothing written. Writes atomically, exits 0. A
      failed write is exit 1 with a message.
  store   $TEST_PASS_RECORD_DIR if set and non-empty, else
          <absolute git common dir>/test-pass-record
  file    <store>/<sha256(ID) hex>/<T>.json = {"version":1,"tree":T,"test":ID}

ANTI-VACUITY: `test_record_then_check_same_tree_and_id_passes` is the control.
A script that always exited 1 would pass every "fails" row below; that test is
what proves the 1s are about the input.

What this file does NOT prove: that the write is atomic under a concurrent
reader (only that no temp file is left behind), and anything about sha256
repositories beyond the hex-length check.
"""

from __future__ import annotations

import hashlib
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SCRIPT = Path(
    os.environ.get("TEST_PASS_RECORD_UNDER_TEST", _HERE / "test-pass-record.py")
).resolve()


def _which(name):
    path = shutil.which(name)
    if path is None:  # pragma: no cover - environment defect
        raise unittest.SkipTest("required tool not on PATH: " + name)
    return path


class Repo:
    """A throwaway git repo with two distinct trees, one commit, one blob."""

    def __init__(self):
        self.base = Path(tempfile.mkdtemp(prefix="test-pass-record-")).resolve()
        self.root = self.base / "repo"
        self.root.mkdir()
        self.store = self.base / "store"
        self.env = {
            k: v for k, v in os.environ.items()
            if k not in ("TEST_PASS_RECORD_DIR",) and not k.startswith("GIT_")
        }
        self.env.update(
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CEILING_DIRECTORIES=str(self.base),
            GIT_AUTHOR_NAME="t",
            GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t",
            GIT_COMMITTER_EMAIL="t@example.invalid",
        )
        self.git("init", "-q")
        (self.root / "a.txt").write_text("a\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "one")
        self.commit1 = self.git("rev-parse", "HEAD")
        self.tree1 = self.git("rev-parse", "HEAD^{tree}")
        self.blob = self.git("rev-parse", "HEAD:a.txt")
        (self.root / "b.txt").write_text("b\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "two")
        self.tree2 = self.git("rev-parse", "HEAD^{tree}")
        assert self.tree1 != self.tree2

    def git(self, *args):
        return subprocess.run(
            [_which("git"), *args], cwd=str(self.root), env=self.env,
            capture_output=True, text=True, check=True,
        ).stdout.strip()

    def run(self, *args, store="<default-store>", cwd=None):
        env = dict(self.env)
        if store == "<default-store>":
            env["TEST_PASS_RECORD_DIR"] = str(self.store)
        elif store is not None:
            env["TEST_PASS_RECORD_DIR"] = store
        return subprocess.run(
            [sys.executable, str(_SCRIPT), *args],
            cwd=str(cwd or self.root), env=env, capture_output=True, text=True,
        )

    def record(self, tree, test, **kw):
        return self.run("record", "--tree", tree, "--test", test, **kw)

    def check(self, tree, test, **kw):
        return self.run("check", "--tree", tree, "--test", test, **kw)

    def path_for(self, tree, test, store=None):
        return Path(store or self.store) / hashlib.sha256(test.encode()).hexdigest() / (tree + ".json")

    def files_in_store(self):
        if not self.store.exists():
            return []
        return sorted(str(p.relative_to(self.store)) for p in self.store.rglob("*") if p.is_file())

    def cleanup(self):
        shutil.rmtree(self.base, ignore_errors=True)


def _out(proc):
    return "--- exit %d ---\n--- stdout ---\n%s\n--- stderr ---\n%s" % (
        proc.returncode, proc.stdout, proc.stderr)


class PassRecordTestCase(unittest.TestCase):
    ID = "cargo-check:crates/foo"

    def setUp(self):
        if not _SCRIPT.is_file():
            self.fail("script under test does not exist: %s" % _SCRIPT)
        self.r = Repo()
        self.addCleanup(self.r.cleanup)

    def assertOneReasonLine(self, proc):
        lines = [ln for ln in proc.stderr.splitlines() if ln.strip()]
        self.assertEqual(len(lines), 1, "expected exactly one stderr reason line\n" + _out(proc))

    def write_raw(self, tree, test, content):
        p = self.r.path_for(tree, test)
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(content)
        return p


class RecordThenCheck(PassRecordTestCase):
    def test_record_then_check_same_tree_and_id_passes(self):
        rec = self.r.record(self.r.tree1, self.ID)
        self.assertEqual(rec.returncode, 0, _out(rec))
        chk = self.r.check(self.r.tree1, self.ID)
        self.assertEqual(chk.returncode, 0, _out(chk))

    def test_record_writes_the_specified_file_and_content(self):
        rec = self.r.record(self.r.tree1, self.ID)
        self.assertEqual(rec.returncode, 0, _out(rec))
        p = self.r.path_for(self.r.tree1, self.ID)
        self.assertTrue(p.is_file(), "record not at %s; store holds %r" % (p, self.r.files_in_store()))
        self.assertEqual(
            json.loads(p.read_text()),
            {"version": 1, "tree": self.r.tree1, "test": self.ID},
        )
        # atomic write via a temp file in the same dir: nothing else is left behind
        self.assertEqual(self.r.files_in_store(), [str(p.relative_to(self.r.store))])

    def test_a_record_written_by_hand_in_the_spec_format_is_honoured(self):
        """The file format is the contract the hook relies on, not an internal
        detail of `record`."""
        self.write_raw(self.r.tree1, self.ID,
                       json.dumps({"version": 1, "tree": self.r.tree1, "test": self.ID}))
        chk = self.r.check(self.r.tree1, self.ID)
        self.assertEqual(chk.returncode, 0, _out(chk))

    def test_different_tree_fails(self):
        self.assertEqual(self.r.record(self.r.tree1, self.ID).returncode, 0)
        chk = self.r.check(self.r.tree2, self.ID)
        self.assertEqual(chk.returncode, 1, _out(chk))

    def test_different_test_id_fails(self):
        self.assertEqual(self.r.record(self.r.tree1, self.ID).returncode, 0)
        chk = self.r.check(self.r.tree1, "cargo-check:workspace")
        self.assertEqual(chk.returncode, 1, _out(chk))

    def test_missing_record_fails(self):
        chk = self.r.check(self.r.tree1, self.ID)
        self.assertEqual(chk.returncode, 1, _out(chk))

    def test_empty_store_env_falls_back_to_the_git_common_dir(self):
        rec = self.r.record(self.r.tree1, self.ID, store="")
        self.assertEqual(rec.returncode, 0, _out(rec))
        default = self.r.root / ".git" / "test-pass-record"
        self.assertTrue(self.r.path_for(self.r.tree1, self.ID, store=default).is_file(),
                        "record not under %s" % default)
        self.assertEqual(self.r.check(self.r.tree1, self.ID, store=None).returncode, 0)

    def test_unset_store_env_uses_the_absolute_git_common_dir_from_a_subdir(self):
        sub = self.r.root / "sub"
        sub.mkdir()
        rec = self.r.record(self.r.tree1, self.ID, store=None, cwd=sub)
        self.assertEqual(rec.returncode, 0, _out(rec))
        default = self.r.root / ".git" / "test-pass-record"
        self.assertTrue(self.r.path_for(self.r.tree1, self.ID, store=default).is_file())
        self.assertFalse((sub / ".git").exists(), "a relative common dir leaked into cwd")


class CheckRejectsBadRecords(PassRecordTestCase):
    def test_corrupt_non_json_record_fails_with_reason(self):
        self.write_raw(self.r.tree1, self.ID, "{not json")
        chk = self.r.check(self.r.tree1, self.ID)
        self.assertEqual(chk.returncode, 1, _out(chk))
        self.assertOneReasonLine(chk)

    def test_wrong_version_fails_with_reason(self):
        for v in (2, 0, "1", None):
            with self.subTest(version=v):
                self.write_raw(self.r.tree1, self.ID,
                               json.dumps({"version": v, "tree": self.r.tree1, "test": self.ID}))
                chk = self.r.check(self.r.tree1, self.ID)
                self.assertEqual(chk.returncode, 1, _out(chk))
                self.assertOneReasonLine(chk)

    def test_wrong_shape_fails_with_reason(self):
        for body in ("[]", "1", '"x"', json.dumps({"version": 1, "tree": self.r.tree1})):
            with self.subTest(body=body):
                self.write_raw(self.r.tree1, self.ID, body)
                chk = self.r.check(self.r.tree1, self.ID)
                self.assertEqual(chk.returncode, 1, _out(chk))
                self.assertOneReasonLine(chk)

    def test_stored_tree_mismatching_the_filename_fails_with_reason(self):
        self.write_raw(self.r.tree1, self.ID,
                       json.dumps({"version": 1, "tree": self.r.tree2, "test": self.ID}))
        chk = self.r.check(self.r.tree1, self.ID)
        self.assertEqual(chk.returncode, 1, _out(chk))
        self.assertOneReasonLine(chk)

    def test_stored_test_mismatching_the_request_fails_with_reason(self):
        self.write_raw(self.r.tree1, self.ID,
                       json.dumps({"version": 1, "tree": self.r.tree1, "test": "compile-fail"}))
        chk = self.r.check(self.r.tree1, self.ID)
        self.assertEqual(chk.returncode, 1, _out(chk))
        self.assertOneReasonLine(chk)

    def test_unreadable_record_fails_with_reason(self):
        p = self.write_raw(self.r.tree1, self.ID, "")
        p.unlink()
        p.mkdir()  # a directory where the file should be: open() fails
        chk = self.r.check(self.r.tree1, self.ID)
        self.assertEqual(chk.returncode, 1, _out(chk))
        self.assertOneReasonLine(chk)

    def test_non_hex_or_abbreviated_tree_fails_with_reason(self):
        # A valid record exists for tree1, so only the malformed request can fail it.
        self.assertEqual(self.r.record(self.r.tree1, self.ID).returncode, 0)
        for bad in (self.r.tree1[:12], self.r.tree1.upper(), "g" * 40,
                    self.r.tree1 + "0", "HEAD", "../" + self.r.tree1[3:]):
            with self.subTest(tree=bad):
                chk = self.r.check(bad, self.ID)
                self.assertEqual(chk.returncode, 1, _out(chk))
                self.assertOneReasonLine(chk)

    def test_unresolvable_store_fails_with_reason(self):
        outside = self.r.base / "not-a-repo"
        outside.mkdir()
        chk = self.r.check(self.r.tree1, self.ID, store=None, cwd=outside)
        self.assertEqual(chk.returncode, 1, _out(chk))
        self.assertOneReasonLine(chk)


class RecordRejectsNonTrees(PassRecordTestCase):
    def test_commit_sha_is_rejected_and_nothing_written(self):
        rec = self.r.record(self.r.commit1, self.ID)
        self.assertEqual(rec.returncode, 2, _out(rec))
        self.assertEqual(self.r.files_in_store(), [])

    def test_blob_sha_is_rejected_and_nothing_written(self):
        rec = self.r.record(self.r.blob, self.ID)
        self.assertEqual(rec.returncode, 2, _out(rec))
        self.assertEqual(self.r.files_in_store(), [])

    def test_malformed_tree_is_rejected_and_nothing_written(self):
        for bad in (self.r.tree1[:12], self.r.tree1.upper(), "HEAD^{tree}", "f" * 40):
            with self.subTest(tree=bad):
                rec = self.r.record(bad, self.ID)
                self.assertEqual(rec.returncode, 2, _out(rec))
                self.assertEqual(self.r.files_in_store(), [])

    def test_failed_write_exits_one_with_message(self):
        blocker = self.r.base / "store-is-a-file"
        blocker.write_text("x")
        rec = self.r.record(self.r.tree1, self.ID, store=str(blocker))
        self.assertEqual(rec.returncode, 1, _out(rec))
        self.assertTrue(rec.stderr.strip(), "a failed write must say so\n" + _out(rec))


if __name__ == "__main__":
    unittest.main(verbosity=2)
