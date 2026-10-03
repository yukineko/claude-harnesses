#!/usr/bin/env python3
"""Behavioural test for backlog 233f819c: stop-verify-worktree.py must not block a
session's stop on MAIN-tree dirt that a DIFFERENT concurrent session wrote
(CLAUDE.md section 8 attribution bug).

INTERFACE SPEC (the implementer must provide this; the test is the oracle)
==========================================================================
New module scripts/edit_ledger.py (sibling of scripts/deny_ledger.py):

  Location. Directory = $HARNESS_EDIT_LEDGER_DIR if set (tests redirect it),
  else ~/.claude/state/maintree-edits. One file per session:
  <dir>/<session_id>.jsonl. session_id must pass the same validation as
  deny_ledger.session_of (an unusable one is Undetermined, never silently
  skipped).

  On-disk format. JSON Lines, one object per line:
      {"session_id": "<sid>", "root": "<realpath of main checkout toplevel>",
       "path": "<path relative to root, posix separators>", "ts": <float epoch>}
  Any line that is not a JSON object with those string fields, or whose
  session_id differs from the file name, makes the ledger CORRUPT.

  API.
    record(session_id: str, repo_root: str, paths: list[str]) -> None
        Append one entry per path (paths absolute or root-relative; stored
        root-relative). Called by guard-maintree-edit.py and
        guard-maintree-bash.py ONLY for operations they ALLOW on the main tree
        (e.g. while MERGE_HEAD is present); a refused edit wrote nothing.
        Raises on IO failure (callers resolve a failed record to refuse).
    owners(repo_root: str) -> Determination-like result
        Reads ALL session files in the ledger dir. Known(dict[path -> set of
        session_ids]) when every file was read and parsed; Undetermined (never
        an empty dict) when the dir/any file is unreadable or corrupt. An
        absent/empty ledger dir is Known({}).

  stop-verify-worktree.py contract (stopping session = payload.session_id):
  a dirty main-tree path (git status --porcelain) is EXCLUDED from blocking
  only when owners() is Known, the path has a recorded owner set, that set
  does not contain the stopping session, and is non-empty. Everything else
  blocks (exit 2, stderr names the path): unrecorded path, path written by the
  stopping session (alone or together with others), corrupt/unreadable ledger,
  owners() raising.
==========================================================================
Run: python3 scripts/test_stop_verify_worktree_attribution_233f819c.py
"""
import json
import os
import subprocess
import sys
import tempfile
import unittest

HERE = os.path.dirname(os.path.abspath(__file__))
STOP = os.path.join(HERE, "stop-verify-worktree.py")
EDIT_GUARD = os.path.join(HERE, "guard-maintree-edit.py")
A, B = "sess-A", "sess-B"


def sh(cwd, *args, env=None):
    r = subprocess.run(args, cwd=cwd, capture_output=True, text=True, env=env)
    if r.returncode != 0:
        raise RuntimeError(f"{args} failed: {r.stderr}")
    return r.stdout


class Case(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory()
        self.addCleanup(self._td.cleanup)
        self.tmp = os.path.realpath(self._td.name)
        self.repo = os.path.join(self.tmp, "repo")
        self.home = os.path.join(self.tmp, "home")
        self.ledger = os.path.join(self.tmp, "ledger")
        for d in (self.repo, self.home, self.ledger):
            os.makedirs(d)
        sh(self.repo, "git", "init", "-q", "-b", "main")
        for f in ("one.txt", "two.txt"):
            with open(os.path.join(self.repo, f), "w") as fh:
                fh.write("base\n")
        sh(self.repo, "git", "add", ".")
        sh(self.repo, "git", "-c", "user.name=t", "-c", "user.email=t@t",
           "commit", "-q", "-m", "init")

    def env(self):
        e = dict(os.environ)
        e.update(HOME=self.home, HARNESS_EDIT_LEDGER_DIR=self.ledger,
                 CLAUDE_PROJECT_DIR=self.repo)
        for k in ("CLAUDE_CODE_ENTRYPOINT", "CLAUDECODE"):
            e.pop(k, None)
        return e

    def dirty(self, *names):
        for n in names:
            with open(os.path.join(self.repo, n), "a") as fh:
                fh.write("dirty\n")

    def seed(self, sid, path, root=None):
        with open(os.path.join(self.ledger, sid + ".jsonl"), "a") as fh:
            fh.write(json.dumps({"session_id": sid, "root": root or self.repo,
                                 "path": path, "ts": 1.0}) + "\n")

    def stop(self, sid=A):
        payload = {"session_id": sid, "cwd": self.repo, "stop_hook_active": False}
        r = subprocess.run([sys.executable, STOP], input=json.dumps(payload),
                           capture_output=True, text=True, env=self.env(),
                           cwd=self.repo)
        return r.returncode, r.stderr

    # -- cases ---------------------------------------------------------
    def test_other_session_only_allows(self):
        self.dirty("one.txt")
        self.seed(B, "one.txt")
        rc, err = self.stop()
        self.assertEqual(rc, 0, f"B-only dirt must not block A. stderr={err}")

    def test_self_written_blocks(self):
        self.dirty("one.txt")
        self.seed(A, "one.txt")
        rc, err = self.stop()
        self.assertEqual(rc, 2)
        self.assertIn("one.txt", err)

    def test_both_sessions_blocks(self):
        self.dirty("one.txt")
        self.seed(A, "one.txt")
        self.seed(B, "one.txt")
        rc, err = self.stop()
        self.assertEqual(rc, 2, err)

    def test_unrecorded_blocks(self):
        self.dirty("one.txt")
        rc, err = self.stop()
        self.assertEqual(rc, 2, err)

    def test_corrupt_ledger_blocks(self):
        self.dirty("one.txt")
        self.seed(B, "one.txt")
        with open(os.path.join(self.ledger, B + ".jsonl"), "a") as fh:
            fh.write("{not json\n")
        rc, err = self.stop()
        self.assertEqual(rc, 2, err)

    @unittest.skipIf(hasattr(os, "geteuid") and os.geteuid() == 0, "root reads anything")
    def test_unreadable_ledger_blocks(self):
        self.dirty("one.txt")
        self.seed(B, "one.txt")
        p = os.path.join(self.ledger, B + ".jsonl")
        os.chmod(p, 0)
        self.addCleanup(os.chmod, p, 0o644)
        rc, err = self.stop()
        self.assertEqual(rc, 2, err)

    def test_mixed_b_only_and_unrecorded_blocks(self):
        self.dirty("one.txt", "two.txt")
        self.seed(B, "one.txt")
        rc, err = self.stop()
        self.assertEqual(rc, 2, err)
        self.assertIn("two.txt", err)

    def test_guard_edit_records_session_b(self):
        """Seed via the real PreToolUse guard: session B edits one.txt while a
        merge is in progress on main (the only allowed main-tree edit), so the
        guard must record B as the writer; A's stop must then be allowed."""
        git_dir = os.path.join(self.repo, ".git")
        with open(os.path.join(git_dir, "MERGE_HEAD"), "w") as fh:
            fh.write(sh(self.repo, "git", "rev-parse", "HEAD"))
        target = os.path.join(self.repo, "one.txt")
        payload = {"session_id": B, "cwd": self.repo, "tool_name": "Edit",
                   "tool_input": {"file_path": target, "old_string": "base",
                                  "new_string": "edited"}}
        r = subprocess.run([sys.executable, EDIT_GUARD], input=json.dumps(payload),
                           capture_output=True, text=True, env=self.env(),
                           cwd=self.repo)
        self.assertEqual(r.returncode, 0, f"guard should allow merge edit: {r.stderr}")
        self.dirty("one.txt")  # the edit tool itself is not run by the guard
        rc, err = self.stop()
        self.assertEqual(rc, 0, f"guard did not record B as writer. stderr={err}")


if __name__ == "__main__":
    unittest.main()
