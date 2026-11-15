#!/usr/bin/env python3
"""Gates judge only their OWN tree: they never look at, verify or test another worktree.

User ruling 2026-11-08 (verbatim):

    "donegateそのものが他のworktreeを見たり検証したりテストしたりすることは禁止とする。
    他のgateでも禁止とする。合理的理由があって確認するなら、それを提示すること。基本的に認めない"

    "おそらく他のworktreeへの干渉をやめないと並行性があがるほど、処理する時間が膨大になる、
    そこに合理的理由はない。定期的にまとまったauditの実施での担保とする。"

Contract under test (scripts/guard-maintree-bash.py, scripts/guard-maintree-edit.py,
scripts/stop-verify-worktree.py, helper scripts/deny_ledger.py):

A. Bash guard. The tree a Bash call runs in is the toplevel of the PRE payload `cwd`.
   Only when that is the project's MAIN tree is main snapshotted at Pre and compared
   at Post. When cwd is a linked worktree, Pre writes only a small marker (still under
   <main>/.git/maintree-guard/, keyed by session + tool_use_id) and Post exits 0 WITHOUT
   reading main -- even if main changed during the call (another session) and even if
   main's git is unusable.
B. Stop gate. Blocks (exit 2) only on (i) dirt in main at paths recorded in THIS
   session's own write record, or (ii) deny-ledger targets that changed -- and only
   when the Stop payload cwd is main. Write record =
   `$HOME/.claude/state/maintree-writes/<session_id>.jsonl` (JSON lines), filled by the
   hooks: guard-maintree-bash.py Post (cwd main) records the paths that changed during
   the call; guard-maintree-edit.py records an ALLOWED edit to a main path. Absent
   record = this session wrote nothing. Unreadable or corrupt record of THIS session
   -> block. With a linked-worktree cwd nothing in main is inspected -> exit 0.
C. Edit guard. "Own tree" = the tree the TARGET belongs to, by path. A merge in
   progress in main lifts the main refusal only for a call whose cwd is main.

Every hook is driven as a real subprocess with a JSON payload on stdin, in a
throwaway repo (main tree + linked worktree), with HOME, TMPDIR and
STOP_VERIFY_WORKTREE_STATE_DIR pointed at temp dirs and a clean GIT_* env.
Each test uses its own session_id. Stop payloads carry session_id, cwd,
hook_event_name and stop_hook_active (a real Stop payload has no tool_name /
tool_input, so none is invented here).

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.

    python3 -m pytest -q scripts/test_gate_own_tree_only.py
"""
from __future__ import annotations

import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
BASH = SCRIPTS / "guard-maintree-bash.py"
EDIT = SCRIPTS / "guard-maintree-edit.py"
STOP = SCRIPTS / "stop-verify-worktree.py"

GIT_ID = ["-c", "user.name=t", "-c", "user.email=t@example.invalid",
          "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"]


def _clean_env() -> dict:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    env["GIT_CONFIG_GLOBAL"] = os.devnull
    env["GIT_AUTHOR_NAME"] = env["GIT_COMMITTER_NAME"] = "t"
    env["GIT_AUTHOR_EMAIL"] = env["GIT_COMMITTER_EMAIL"] = "t@example.invalid"
    return env


def git(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *GIT_ID, *args], cwd=cwd, env=_clean_env(),
                          capture_output=True, text=True, check=check)


class Base(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="gate-own-tree-")))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.home = self.tmp / "home"
        self.home.mkdir()
        self.tmpdir = self.tmp / "tmpdir"
        self.tmpdir.mkdir()
        self.svw_state = self.tmp / "svw-state"
        self.main = self.tmp / "main"
        self.main.mkdir()
        git(self.main, "init", "-q", "-b", "main")
        (self.main / "tracked.txt").write_text("v1\n")
        (self.main / "other.txt").write_text("o1\n")
        git(self.main, "add", "-A")
        git(self.main, "commit", "-q", "-m", "init")
        self.wt = self.tmp / "wt"
        git(self.main, "worktree", "add", "-q", "-b", "wtbranch", str(self.wt))
        self.guard_dir = self.main / ".git" / "maintree-guard"
        self.sid = f"own-tree-{self.id().rsplit('.', 1)[-1]}"
        self._n = 0

    # -- environment / invocation -------------------------------------------
    def env(self) -> dict:
        env = _clean_env()
        env["HOME"] = str(self.home)
        env["TMPDIR"] = str(self.tmpdir)
        env["STOP_VERIFY_WORKTREE_STATE_DIR"] = str(self.svw_state)
        env["CLAUDE_PROJECT_DIR"] = str(self.main)
        env["CLAUDECODE"] = "1"
        env["CLAUDE_CODE_ENTRYPOINT"] = "sdk-cli"
        return env

    def _run(self, script: Path, payload: dict, cwd: Path) -> subprocess.CompletedProcess:
        return subprocess.run([sys.executable, str(script)], input=json.dumps(payload),
                              cwd=str(cwd), env=self.env(), capture_output=True,
                              text=True, timeout=120)

    def _tid(self) -> str:
        self._n += 1
        return f"toolu_owntree_{self._n:04d}"

    def bash(self, event: str, tid: str, cwd: Path, command: str = "true"):
        payload = {"hook_event_name": event, "session_id": self.sid,
                   "tool_use_id": tid, "tool_name": "Bash",
                   "tool_input": {"command": command}, "cwd": str(cwd)}
        if event == "PostToolUse":
            payload["tool_response"] = {"stdout": "", "stderr": "", "interrupted": False}
        return self._run(BASH, payload, cwd)

    def edit(self, target: Path, cwd: Path):
        payload = {"hook_event_name": "PreToolUse", "session_id": self.sid,
                   "tool_name": "Write", "cwd": str(cwd),
                   "tool_input": {"file_path": str(target), "content": "edited\n"}}
        return self._run(EDIT, payload, cwd)

    def stop(self, cwd: Path):
        payload = {"hook_event_name": "Stop", "session_id": self.sid,
                   "cwd": str(cwd), "stop_hook_active": False}
        return self._run(STOP, payload, cwd)

    # -- observation helpers ------------------------------------------------
    def guard_files(self) -> set[Path]:
        if not self.guard_dir.exists():
            return set()
        return {p for p in self.guard_dir.rglob("*") if p.is_file()}

    def status(self, tree: Path | None = None) -> str:
        return git(tree or self.main, "status", "--porcelain", "--untracked-files=all").stdout

    def record(self) -> Path:
        return self.home / ".claude" / "state" / "maintree-writes" / f"{self.sid}.jsonl"

    def deny_ledger_file(self) -> Path:
        return self.home / ".claude" / "state" / "maintree-deny" / f"{self.sid}.jsonl"

    def assert_rc(self, r: subprocess.CompletedProcess, rc: int, what: str) -> None:
        self.assertEqual(r.returncode, rc,
                         f"{what}: expected exit {rc}, got {r.returncode}; "
                         f"stdout={r.stdout!r} stderr={r.stderr!r}")

    def pre_with_marker(self, cwd: Path) -> str:
        """Pre must allow and must leave a file under <main>/.git/maintree-guard/."""
        tid = self._tid()
        before = self.guard_files()
        self.assert_rc(self.bash("PreToolUse", tid, cwd), 0, "Bash Pre")
        self.assertTrue(self.guard_files() - before,
                        f"anti-vacuity: Pre wrote nothing under {self.guard_dir}")
        return tid

    def merge_head_oid(self) -> str:
        """A real commit oid (on the worktree branch) written as main's MERGE_HEAD."""
        (self.wt / "side.txt").write_text("side\n")
        git(self.wt, "add", "side.txt")
        git(self.wt, "commit", "-q", "-m", "side")
        oid = git(self.wt, "rev-parse", "HEAD").stdout.strip()
        (self.main / ".git" / "MERGE_HEAD").write_text(oid + "\n")
        self.assertTrue((self.main / ".git" / "MERGE_HEAD").is_file())
        return oid


# ---------------------------------------------------------------------------
# A. Bash guard
# ---------------------------------------------------------------------------
class A_BashGuard(Base):
    def test_A1_cwd_worktree_ignores_foreign_change_in_main(self) -> None:
        """cwd=wt; another session adds a file and modifies a tracked file in main
        between Pre and Post -> Post exit 0.

        Does NOT prove Post never READ main (only that it did not refuse); does not
        prove a write into main by the command itself from a wt cwd is caught (by
        the new contract it is not)."""
        tid = self.pre_with_marker(self.wt)
        (self.main / "foreign-new.txt").write_text("other session\n")
        (self.main / "tracked.txt").write_text("other session edit\n")
        st = self.status()
        self.assertIn("foreign-new.txt", st, "anti-vacuity: main change landed")
        self.assertIn("tracked.txt", st, "anti-vacuity: main change landed")
        self.assert_rc(self.bash("PostToolUse", tid, self.wt), 0, "Bash Post (cwd=wt)")

    def test_A2_cwd_worktree_main_git_unusable_after_pre(self) -> None:
        """cwd=wt; main's index overwritten with garbage after Pre -> Pre 0, Post 0.

        Does NOT prove anything about git failures at Pre (see A2b), nor that the
        rest of main's .git (refs, HEAD) is never read."""
        tid = self.pre_with_marker(self.wt)
        (self.main / ".git" / "index").write_bytes(b"garbage-not-an-index" * 8)
        probe = git(self.main, "status", "--porcelain", check=False)
        self.assertNotEqual(probe.returncode, 0,
                            "anti-vacuity: main's git status must really be unusable")
        self.assert_rc(self.bash("PostToolUse", tid, self.wt), 0, "Bash Post (cwd=wt)")

    def test_A2b_cwd_worktree_main_index_garbage_before_pre(self) -> None:
        """cwd=wt; main's index is garbage BEFORE Pre -> Pre 0 (marker written), Post 0.

        Does NOT prove main's .git directory is not touched at all (the marker lives
        under it by contract)."""
        (self.main / ".git" / "index").write_bytes(b"garbage-not-an-index" * 8)
        probe = git(self.main, "status", "--porcelain", check=False)
        self.assertNotEqual(probe.returncode, 0,
                            "anti-vacuity: main's git status must really be unusable")
        tid = self.pre_with_marker(self.wt)
        self.assert_rc(self.bash("PostToolUse", tid, self.wt), 0, "Bash Post (cwd=wt)")

    def test_A3_cwd_main_write_into_main_still_refused(self) -> None:
        """cwd=main; the call modifies a tracked file in main -> Post exit 2 naming it.

        Does NOT prove the refusal is attributed to this call rather than a
        concurrent session (the guard cannot tell)."""
        tid = self.pre_with_marker(self.main)
        (self.main / "tracked.txt").write_text("written by this call\n")
        self.assertIn("tracked.txt", self.status())
        r = self.bash("PostToolUse", tid, self.main)
        self.assert_rc(r, 2, "Bash Post (cwd=main, wrote main)")
        self.assertIn("tracked.txt", r.stderr)

    def test_A4_cwd_main_preexisting_dirt_unchanged_allowed(self) -> None:
        """cwd=main; main already dirty (untracked + modified) before Pre by another
        session; the call changes nothing -> Post 0.

        Does NOT prove dirt that changes DURING the call is handled either way."""
        (self.main / "foreign-untracked.txt").write_text("x\n")
        (self.main / "other.txt").write_text("foreign modification\n")
        st = self.status()
        self.assertIn("foreign-untracked.txt", st)
        self.assertIn("other.txt", st)
        tid = self.pre_with_marker(self.main)
        self.assert_rc(self.bash("PostToolUse", tid, self.main), 0, "Bash Post (cwd=main)")


# ---------------------------------------------------------------------------
# B. Stop gate
# ---------------------------------------------------------------------------
class B_StopGate(Base):
    def test_B1_cwd_main_foreign_dirt_no_record_allowed(self) -> None:
        """Stop cwd=main; main dirty by another session; this session has no write
        record -> 0.

        Does NOT prove a record of a DIFFERENT session is ignored (none is written)."""
        (self.main / "foreign.txt").write_text("x\n")
        (self.main / "tracked.txt").write_text("foreign\n")
        self.assertIn("foreign.txt", self.status(), "anti-vacuity: main is dirty")
        self.assertFalse(self.record().exists(), "precondition: no write record")
        self.assert_rc(self.stop(self.main), 0, "Stop (cwd=main, foreign dirt)")

    def test_B2_cwd_worktree_main_dirty_allowed(self) -> None:
        """Stop cwd=wt; main dirty -> 0.

        Does NOT prove main is not inspected (only that it did not block)."""
        (self.main / "foreign.txt").write_text("x\n")
        self.assertIn("foreign.txt", self.status(), "anti-vacuity: main is dirty")
        self.assert_rc(self.stop(self.wt), 0, "Stop (cwd=wt)")

    def _bash_write_new_in_main(self) -> None:
        tid = self.pre_with_marker(self.main)
        (self.main / "new.txt").write_text("this session\n")
        r = self.bash("PostToolUse", tid, self.main)
        self.assert_rc(r, 2, "Bash Post (cwd=main, wrote new.txt)")
        self.assertIn("new.txt", self.status())

    def test_B3_own_bash_write_in_main_blocks_stop(self) -> None:
        """This session's Bash call (cwd=main) wrote main/new.txt -> Stop cwd=main 2.

        Does NOT prove the block comes from the write record rather than any dirt
        in main (old behaviour blocks on any dirt too); B1 + B8 discriminate."""
        self._bash_write_new_in_main()
        self.assert_rc(self.stop(self.main), 2, "Stop (cwd=main, own write dirty)")

    def test_B4_refused_edit_then_changed_blocks_stop(self) -> None:
        """Edit to main refused (cwd=main, no merge); the same file is then changed
        by another spelling -> Stop cwd=main 2.

        Does NOT prove which signal (ledger or write record) produced the block."""
        target = self.main / "tracked.txt"
        self.assert_rc(self.edit(target, self.main), 2, "Edit into main (no merge)")
        target.write_text("changed anyway\n")
        self.assertIn("tracked.txt", self.status())
        self.assert_rc(self.stop(self.main), 2, "Stop (cwd=main, refused-then-changed)")

    def test_B5_refused_edit_then_changed_stop_in_worktree_allowed(self) -> None:
        """Same as B4 but Stop cwd=wt -> 0 (nothing in main inspected).

        Does NOT prove the ledger is not READ, only that it does not block."""
        target = self.main / "tracked.txt"
        self.assert_rc(self.edit(target, self.main), 2, "Edit into main (no merge)")
        self.assertTrue(self.deny_ledger_file().exists(),
                        "anti-vacuity: the refusal was recorded in the deny ledger")
        target.write_text("changed anyway\n")
        self.assertIn("tracked.txt", self.status(), "anti-vacuity: main changed")
        self.assert_rc(self.stop(self.wt), 0, "Stop (cwd=wt)")

    def test_B6_corrupt_own_record_blocks(self) -> None:
        """This session's write record holds a non-JSON line; main clean;
        Stop cwd=main -> 2.

        Does NOT prove any bound/aside handling of the corrupt record."""
        self.record().parent.mkdir(parents=True, exist_ok=True)
        self.record().write_text("not json\n")
        self.assertEqual(self.status(), "", "precondition: main is clean")
        self.assert_rc(self.stop(self.main), 2, "Stop (cwd=main, corrupt record)")

    def test_B7_unreadable_own_record_blocks(self) -> None:
        """This session's write record is mode 000; main clean; Stop cwd=main -> 2.

        Does NOT prove the block's message names the record."""
        if hasattr(os, "geteuid") and os.geteuid() == 0:
            self.skipTest("root can read mode-000 files")
        rec = self.record()
        rec.parent.mkdir(parents=True, exist_ok=True)
        rec.write_text(json.dumps({"paths": ["tracked.txt"]}) + "\n")
        rec.chmod(0)
        self.addCleanup(rec.chmod, 0o600)
        self.assertFalse(os.access(rec, os.R_OK), "anti-vacuity: record unreadable")
        self.assertEqual(self.status(), "", "precondition: main is clean")
        self.assert_rc(self.stop(self.main), 2, "Stop (cwd=main, unreadable record)")

    def test_B8_own_write_reverted_or_committed_allows(self) -> None:
        """As B3, then new.txt is committed so main is clean -> Stop cwd=main 0.

        Anti-vacuity: the write record exists and names new.txt (so the 0 is not
        'nothing was recorded'). Does NOT prove the record format beyond 'contains
        the path'."""
        self._bash_write_new_in_main()
        self.assertTrue(self.record().exists(), "anti-vacuity: write record exists")
        self.assertIn("new.txt", self.record().read_text(),
                      "anti-vacuity: write record names new.txt")
        git(self.main, "add", "new.txt")
        git(self.main, "commit", "-q", "-m", "commit new.txt")
        self.assertEqual(self.status(), "", "main is clean again")
        self.assert_rc(self.stop(self.main), 0, "Stop (cwd=main, own write committed)")


# ---------------------------------------------------------------------------
# C. Edit guard
# ---------------------------------------------------------------------------
class C_EditGuard(Base):
    def test_C1_cwd_worktree_target_main_with_merge_head_refused(self) -> None:
        """cwd=wt, target a tracked file in main, main has MERGE_HEAD -> 2.

        Does NOT prove a real conflicted merge behaves the same (a valid oid is
        written into MERGE_HEAD)."""
        self.merge_head_oid()
        target = self.main / "tracked.txt"
        self.assertIn("tracked.txt", git(self.main, "ls-files").stdout)
        self.assert_rc(self.edit(target, self.wt), 2, "Edit main from wt during merge")

    def test_C2_cwd_main_target_main_with_merge_head_allowed_and_recorded(self) -> None:
        """cwd=main, same MERGE_HEAD -> 0; then main dirty at that path ->
        Stop cwd=main 2 (record).

        Does NOT prove the block came from the record rather than generic dirt
        (old behaviour blocks on any dirt); B1 discriminates generic dirt."""
        self.merge_head_oid()
        target = self.main / "tracked.txt"
        self.assert_rc(self.edit(target, self.main), 0, "Edit main from main during merge")
        target.write_text("conflict resolved\n")
        self.assertIn("tracked.txt", self.status(), "anti-vacuity: the edit landed")
        self.assert_rc(self.stop(self.main), 2, "Stop (cwd=main, own merge edit dirty)")

    def test_C3_cwd_main_target_worktree_allowed(self) -> None:
        """cwd=main, target a file in the linked worktree -> 0.

        Does NOT prove the edit guard never runs git in main."""
        target = self.wt / "tracked.txt"
        self.assertIn("tracked.txt", git(self.wt, "ls-files").stdout,
                      "anti-vacuity: target is a tracked file of the worktree")
        self.assert_rc(self.edit(target, self.main), 0, "Edit wt from main")

    def test_C4_cwd_worktree_target_other_worktree_allowed(self) -> None:
        """cwd=wt, target a file in ANOTHER linked worktree (wt2) -> 0.

        Does NOT prove wt2 is not inspected by git."""
        wt2 = self.tmp / "wt2"
        git(self.main, "worktree", "add", "-q", "-b", "wt2branch", str(wt2))
        target = wt2 / "tracked.txt"
        self.assertIn("tracked.txt", git(wt2, "ls-files").stdout,
                      "anti-vacuity: target is a tracked file of wt2")
        self.assert_rc(self.edit(target, self.wt), 0, "Edit wt2 from wt")

    def test_C5_cwd_worktree_target_main_no_merge_refused(self) -> None:
        """cwd=wt, target in main, no merge -> 2.

        Does NOT prove anything about the deny message wording."""
        self.assertFalse((self.main / ".git" / "MERGE_HEAD").exists())
        self.assert_rc(self.edit(self.main / "tracked.txt", self.wt), 2,
                       "Edit main from wt (no merge)")


if __name__ == "__main__":
    unittest.main()
