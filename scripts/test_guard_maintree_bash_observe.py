#!/usr/bin/env python3
"""Behavioural tests for the OBSERVING scripts/guard-maintree-bash.py.

Contract (user decision 2026-10-03): the guard no longer predicts from command
syntax whether a Bash command writes into the main (primary) working tree. It
OBSERVES the effect:

  PreToolUse(Bash)  -> snapshot main (porcelain v2 status incl. untracked, HEAD,
                       MERGE_HEAD presence) under <git-common-dir>/maintree-guard/
                       keyed by session_id + tool_use_id; exit 0 for ANY command
                       text; exit 2 if the snapshot cannot be taken.
  PostToolUse(Bash) -> re-snapshot and compare:
                       a. identical                       -> 0 (snapshot removed)
                       b. differs, MERGE_HEAD after       -> 0
                       c. differs, HEAD moved, clean tree -> 0
                       d. any other difference            -> 2 (names paths, §8)
                       e. no / corrupt snapshot, git fail -> 2
  non-Bash tool     -> 0, no state written.
  Linked-worktree changes never trigger d; gitignored files do not count.

"The command" is simulated by the test itself doing the filesystem/git action
between the Pre and the Post invocation. Every block (d) case asserts the change
really landed in main (so the block is not vacuous) and every allow case asserts
the Pre snapshot was really written (so the allow is not "nothing was checked").

Written by an independent test author (CLAUDE.md 2.(a)); not by the implementer.
"""
from __future__ import annotations

import hashlib
import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

SCRIPT = Path(__file__).resolve().parent / "guard-maintree-bash.py"

GIT_ID = ["-c", "user.name=t", "-c", "user.email=t@example.invalid",
          "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"]


def _clean_env() -> dict:
    env = {k: v for k, v in os.environ.items() if not k.startswith("GIT_")}
    env["GIT_CONFIG_NOSYSTEM"] = "1"
    env["GIT_AUTHOR_NAME"] = env["GIT_COMMITTER_NAME"] = "t"
    env["GIT_AUTHOR_EMAIL"] = env["GIT_COMMITTER_EMAIL"] = "t@example.invalid"
    return env


def git(cwd: Path, *args: str, check: bool = True) -> subprocess.CompletedProcess:
    return subprocess.run(["git", *GIT_ID, *args], cwd=cwd, env=_clean_env(),
                          capture_output=True, text=True, check=check)


class ObserveGuardBase(unittest.TestCase):
    SESSION = "sess-observe-0001"

    def setUp(self) -> None:
        self.tmp = Path(os.path.realpath(tempfile.mkdtemp(prefix="mtg-observe-")))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.main = self.tmp / "main"
        self.main.mkdir()
        git(self.main, "init", "-q", "-b", "main")
        (self.main / "tracked.txt").write_text("v1\n")
        (self.main / "other.txt").write_text("o1\n")
        (self.main / ".gitignore").write_text("target/\n__pycache__/\n")
        git(self.main, "add", "-A")
        git(self.main, "commit", "-q", "-m", "init")
        self.wt = self.tmp / "wt"
        git(self.main, "worktree", "add", "-q", "-b", "wtbranch", str(self.wt))
        self.state_dir = self.main / ".git" / "maintree-guard"
        self._n = 0

    # -- helpers -----------------------------------------------------------
    def _next_id(self) -> str:
        self._n += 1
        return f"toolu_test_{self._n:04d}"

    def run_hook(self, event: str | None, tool_use_id: str, command: str = "true",
                 tool_name: str = "Bash", project_dir: Path | None = None,
                 cwd: Path | None = None, env_override: dict | None = None,
                 session: str | None = None) -> subprocess.CompletedProcess:
        payload = {
            "hook_event_name": event,
            "session_id": session or self.SESSION,
            "tool_use_id": tool_use_id,
            "tool_name": tool_name,
            "tool_input": {"command": command} if tool_name == "Bash"
            else {"file_path": str(self.main / "tracked.txt"), "content": "x"},
            "cwd": str(cwd or self.main),
        }
        if event == "PostToolUse":
            payload["tool_response"] = {"stdout": "", "stderr": "", "interrupted": False}
        elif event == "PostToolUseFailure":
            # Shape observed on a real Claude Code run (2026-10-03, headless probe).
            payload.update({"error": "Exit code 1", "is_interrupt": False,
                            "duration_ms": 12, "permission_mode": "default",
                            "prompt_id": "p-test", "transcript_path": "/dev/null"})
        elif event is None:
            del payload["hook_event_name"]
        env = _clean_env()
        env["CLAUDE_PROJECT_DIR"] = str(project_dir or self.main)
        if env_override:
            env.update(env_override)
        return subprocess.run([sys.executable, str(SCRIPT)],
                              input=json.dumps(payload), cwd=str(cwd or self.main),
                              env=env, capture_output=True, text=True, timeout=60)

    def state_files(self) -> set[Path]:
        if not self.state_dir.exists():
            return set()
        return {p for p in self.state_dir.rglob("*") if p.is_file()}

    def pre_ok(self, command: str = "true", **kw) -> tuple[str, set[Path]]:
        """Run Pre, assert it allowed AND wrote a snapshot. Returns (id, new files)."""
        tid = self._next_id()
        before = self.state_files()
        r = self.run_hook("PreToolUse", tid, command, **kw)
        self.assertEqual(r.returncode, 0, f"Pre must allow any command text: {r.stderr}")
        new = self.state_files() - before
        self.assertTrue(new, "anti-vacuity: Pre must have written a snapshot under "
                             f"{self.state_dir}")
        return tid, new

    def post(self, tid: str, command: str = "true", **kw) -> subprocess.CompletedProcess:
        return self.run_hook("PostToolUse", tid, command, **kw)

    def status(self) -> str:
        return git(self.main, "status", "--porcelain=v2", "--untracked-files=all").stdout

    def _git_stub(self) -> Path:
        d = self.tmp / "stubbin"
        d.mkdir(exist_ok=True)
        g = d / "git"
        g.write_text("#!/bin/sh\necho 'fatal: stub git failure' >&2\nexit 128\n")
        g.chmod(g.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        return d

    def assert_allowed(self, r: subprocess.CompletedProcess) -> None:
        self.assertEqual(r.returncode, 0, f"expected allow; stderr={r.stderr!r}")

    def assert_blocked_d(self, r: subprocess.CompletedProcess, *paths: str) -> None:
        self.assertEqual(r.returncode, 2, f"expected block (d); stderr={r.stderr!r}")
        for p in paths:
            self.assertIn(p, r.stderr, f"stderr must name changed path {p!r}")
        self.assertRegex(r.stderr, r"§\s*8|CLAUDE\.md", "stderr must cite CLAUDE.md §8")
        self.assertIn("session", r.stderr.lower(),
                      "stderr must say another session may be the cause")

    def assert_blocked_e(self, r: subprocess.CompletedProcess) -> None:
        self.assertEqual(r.returncode, 2, f"expected undetermined->block (e); "
                                          f"rc={r.returncode} stderr={r.stderr!r}")
        self.assertTrue(r.stderr.strip(), "a refusal must carry a reason on stderr")


class PreAllowsAnyText(ObserveGuardBase):
    def test_pre_allows_redirect_into_main_text(self) -> None:
        # The old guard refused this at Pre from syntax; the new one never predicts.
        self.pre_ok(f"echo hi > {self.main}/x.txt")

    def test_pre_allows_4a64e6ff_heredoc_and_post_allows_worktree_only_effect(self) -> None:
        out = self.wt / "patched.txt"
        cmd = (f"cd {self.wt} && python3 - <<'PY'\n"
               "from pathlib import Path\n"
               "line = 'python3 \"$REPO/scripts/gate-bypass.py\" --json'\n"
               "Path('patched.txt').write_text(line + '\\n')\n"
               "PY\n")
        tid, _ = self.pre_ok(cmd, cwd=self.wt)
        subprocess.run(["bash", "-c", cmd], check=True, env=_clean_env())
        self.assertIn("$REPO/scripts/gate-bypass.py", out.read_text())
        self.assertFalse((self.main / "scripts").exists())
        self.assert_allowed(self.post(tid, cmd, cwd=self.wt))

    def test_pre_refuses_when_snapshot_impossible_not_a_repo(self) -> None:
        nonrepo = self.tmp / "nonrepo"
        nonrepo.mkdir()
        r = self.run_hook("PreToolUse", self._next_id(), "true",
                          project_dir=nonrepo, cwd=nonrepo)
        self.assert_blocked_e(r)

    def test_pre_refuses_when_state_not_writable(self) -> None:
        # A regular FILE where the state directory must be: nothing can be
        # stored, so the Post comparison would be impossible -> refuse at Pre.
        self.state_dir.write_text("not a directory\n")
        r = self.run_hook("PreToolUse", self._next_id(), "true")
        self.assert_blocked_e(r)

    def test_pre_refuses_when_git_unavailable(self) -> None:
        stub = self._git_stub()
        r = self.run_hook("PreToolUse", self._next_id(), "true",
                          env_override={"PATH": str(stub)})
        self.assert_blocked_e(r)


class BlocksWritesIntoMain(ObserveGuardBase):
    def test_echo_redirect_into_main(self) -> None:
        cmd = f"echo hi > {self.main}/x.txt"
        tid, _ = self.pre_ok(cmd)
        subprocess.run(["sh", "-c", cmd], check=True)
        self.assertTrue((self.main / "x.txt").exists(), "anti-vacuity")
        self.assert_blocked_d(self.post(tid, cmd), "x.txt")

    def test_python_written_file_into_main(self) -> None:
        cmd = (f"python3 -c \"open('{self.main}/py_written.txt','w').write('z')\"")
        tid, _ = self.pre_ok(cmd)
        subprocess.run([sys.executable, "-c",
                        f"open({str(self.main / 'py_written.txt')!r},'w').write('z')"],
                       check=True)
        self.assertTrue((self.main / "py_written.txt").exists(), "anti-vacuity")
        self.assert_blocked_d(self.post(tid, cmd), "py_written.txt")

    def test_modify_tracked_file_in_main(self) -> None:
        tid, _ = self.pre_ok("sed -i '' s/v1/v2/ tracked.txt")
        (self.main / "tracked.txt").write_text("v2\n")
        self.assertIn("tracked.txt", self.status(), "anti-vacuity")
        self.assert_blocked_d(self.post(tid), "tracked.txt")

    def test_delete_tracked_file_in_main(self) -> None:
        tid, _ = self.pre_ok("rm other.txt")
        (self.main / "other.txt").unlink()
        self.assertIn("other.txt", self.status(), "anti-vacuity")
        self.assert_blocked_d(self.post(tid), "other.txt")

    def test_git_add_in_main_staging_is_the_only_delta(self) -> None:
        # The untracked file exists BEFORE Pre, so the only change across the
        # command is the staging itself.
        (self.main / "staged.txt").write_text("s\n")
        tid, _ = self.pre_ok("git add staged.txt")
        before = self.status()
        git(self.main, "add", "staged.txt")
        after = self.status()
        self.assertNotEqual(before, after, "anti-vacuity: git add changed the index")
        self.assert_blocked_d(self.post(tid, "git add staged.txt"), "staged.txt")

    def test_head_moved_by_non_merge_commit_leaving_dirty_tree(self) -> None:
        tid, _ = self.pre_ok("git commit -am x")
        old_head = git(self.main, "rev-parse", "HEAD").stdout.strip()
        (self.main / "tracked.txt").write_text("committed-on-main\n")
        git(self.main, "commit", "-q", "-am", "direct commit on main")
        (self.main / "leftover.txt").write_text("dirty\n")
        self.assertNotEqual(old_head, git(self.main, "rev-parse", "HEAD").stdout.strip())
        self.assertFalse((self.main / ".git" / "MERGE_HEAD").exists())
        self.assertIn("leftover.txt", self.status(), "anti-vacuity: tree is dirty")
        self.assert_blocked_d(self.post(tid), "leftover.txt")

    def test_project_dir_is_linked_worktree_but_main_is_written(self) -> None:
        # CLAUDE_PROJECT_DIR may be a linked worktree; main is still the
        # primary worktree of that repository and is still guarded.
        tid, _ = self.pre_ok(f"echo hi > {self.main}/from_wt.txt",
                             project_dir=self.wt, cwd=self.wt)
        (self.main / "from_wt.txt").write_text("hi\n")
        self.assert_blocked_d(self.post(tid, project_dir=self.wt, cwd=self.wt),
                              "from_wt.txt")


class AllowsNonMainEffects(ObserveGuardBase):
    def test_no_change_allows_and_removes_snapshot(self) -> None:
        tid, new = self.pre_ok("ls")
        self.assert_allowed(self.post(tid, "ls"))
        self.assertFalse(new & self.state_files(),
                         "case a: the stored snapshot must be removed after Post")

    def test_writes_only_inside_linked_worktree(self) -> None:
        tid, _ = self.pre_ok("echo > f; git add f; git commit", cwd=self.wt)
        (self.wt / "wt_file.txt").write_text("w\n")
        (self.wt / "tracked.txt").write_text("changed in wt\n")
        git(self.wt, "add", "wt_file.txt")
        git(self.wt, "commit", "-q", "-am", "wt commit")
        (self.wt / "wt_untracked.txt").write_text("u\n")
        self.assertTrue((self.wt / "wt_file.txt").exists())
        self.assertEqual(self.status(), "", "main must be untouched")
        self.assert_allowed(self.post(tid, cwd=self.wt))

    def test_ignored_files_in_main_do_not_count(self) -> None:
        tid, _ = self.pre_ok("cargo build")
        (self.main / "target" / "debug").mkdir(parents=True)
        (self.main / "target" / "debug" / "bin").write_text("elf\n")
        (self.main / "__pycache__").mkdir()
        (self.main / "__pycache__" / "m.cpython-312.pyc").write_bytes(b"\0")
        self.assertTrue((self.main / "target" / "debug" / "bin").exists(), "anti-vacuity")
        self.assertEqual(self.status(), "", "ignored files are invisible to status")
        self.assert_allowed(self.post(tid))

    def test_merge_in_progress_with_conflict(self) -> None:
        git(self.main, "checkout", "-q", "-b", "side")
        (self.main / "tracked.txt").write_text("side\n")
        git(self.main, "commit", "-q", "-am", "side")
        git(self.main, "checkout", "-q", "main")
        (self.main / "tracked.txt").write_text("mainline\n")
        git(self.main, "commit", "-q", "-am", "mainline")
        tid, _ = self.pre_ok("git merge side")
        r = git(self.main, "merge", "side", check=False)
        self.assertNotEqual(r.returncode, 0, "the merge must conflict")
        self.assertTrue((self.main / ".git" / "MERGE_HEAD").exists(), "anti-vacuity")
        self.assertNotEqual(self.status(), "")
        self.assert_allowed(self.post(tid, "git merge side"))

    def test_conflict_resolution_step_while_merge_in_progress(self) -> None:
        git(self.main, "checkout", "-q", "-b", "side")
        (self.main / "tracked.txt").write_text("side\n")
        git(self.main, "commit", "-q", "-am", "side")
        git(self.main, "checkout", "-q", "main")
        (self.main / "tracked.txt").write_text("mainline\n")
        git(self.main, "commit", "-q", "-am", "mainline")
        git(self.main, "merge", "side", check=False)
        self.assertTrue((self.main / ".git" / "MERGE_HEAD").exists())
        tid, _ = self.pre_ok("resolve + git add tracked.txt")
        (self.main / "tracked.txt").write_text("resolved\n")
        git(self.main, "add", "tracked.txt")
        self.assert_allowed(self.post(tid))

    def test_completed_fast_forward_merge_clean(self) -> None:
        git(self.wt, "commit", "-q", "--allow-empty", "-m", "wt work")
        (self.wt / "new.txt").write_text("n\n")
        git(self.wt, "add", "new.txt")
        git(self.wt, "commit", "-q", "-m", "wt new file")
        old = git(self.main, "rev-parse", "HEAD").stdout.strip()
        tid, _ = self.pre_ok("git merge --ff-only wtbranch")
        git(self.main, "merge", "-q", "--ff-only", "wtbranch")
        self.assertNotEqual(old, git(self.main, "rev-parse", "HEAD").stdout.strip(),
                            "anti-vacuity: HEAD moved")
        self.assertTrue((self.main / "new.txt").exists())
        self.assertEqual(self.status(), "")
        self.assert_allowed(self.post(tid))

    def test_completed_non_ff_merge_commit_clean(self) -> None:
        (self.wt / "w.txt").write_text("w\n")
        git(self.wt, "add", "w.txt")
        git(self.wt, "commit", "-q", "-m", "wt")
        (self.main / "m.txt").write_text("m\n")
        git(self.main, "add", "m.txt")
        git(self.main, "commit", "-q", "-m", "main side")
        tid, _ = self.pre_ok("git merge --no-ff wtbranch")
        git(self.main, "merge", "-q", "--no-ff", "-m", "merge wt", "wtbranch")
        self.assertTrue((self.main / "w.txt").exists(), "anti-vacuity")
        self.assertEqual(self.status(), "")
        self.assert_allowed(self.post(tid))


class OnlyTheDeltaCounts(ObserveGuardBase):
    def _dirty_main(self) -> None:
        (self.main / "tracked.txt").write_text("pre-existing modification\n")
        (self.main / "preexisting_untracked.txt").write_text("u1\n")

    def test_preexisting_dirt_unchanged_allows(self) -> None:
        self._dirty_main()
        tid, _ = self.pre_ok("ls")
        (self.wt / "x.txt").write_text("worktree work\n")
        self.assertNotEqual(self.status(), "", "anti-vacuity: main is dirty")
        self.assert_allowed(self.post(tid))

    def test_preexisting_dirt_plus_new_file_blocks(self) -> None:
        self._dirty_main()
        tid, _ = self.pre_ok("touch more.txt")
        (self.main / "more.txt").write_text("m\n")
        self.assert_blocked_d(self.post(tid), "more.txt")

    def test_preexisting_modified_file_gets_staged_blocks(self) -> None:
        self._dirty_main()
        tid, _ = self.pre_ok("git add tracked.txt")
        git(self.main, "add", "tracked.txt")
        self.assert_blocked_d(self.post(tid), "tracked.txt")

    def test_preexisting_modified_tracked_file_content_changes_further_blocks(self) -> None:
        # NOTE: porcelain v2 reports HEAD/index object names for a modified file,
        # not the worktree content. An implementation that compares ONLY the
        # porcelain text cannot see this change. The property (main changed
        # outside integration) still holds, so it must block.
        self._dirty_main()
        tid, _ = self.pre_ok("echo more >> tracked.txt")
        before = self.status()
        with open(self.main / "tracked.txt", "a") as f:
            f.write("further edit\n")
        self.assertIn("further edit", (self.main / "tracked.txt").read_text(),
                      "anti-vacuity")
        self.assertEqual(before, self.status(),
                         "documents why porcelain alone is blind to this edit")
        self.assert_blocked_d(self.post(tid), "tracked.txt")

    def test_preexisting_untracked_file_content_changes_further_blocks(self) -> None:
        self._dirty_main()
        tid, _ = self.pre_ok("echo more >> preexisting_untracked.txt")
        with open(self.main / "preexisting_untracked.txt", "a") as f:
            f.write("u2\n")
        self.assert_blocked_d(self.post(tid), "preexisting_untracked.txt")


class UndeterminedRefuses(ObserveGuardBase):
    def test_missing_pre_snapshot(self) -> None:
        self.assertEqual(self.status(), "")
        r = self.post("toolu_never_pre_0000")
        self.assert_blocked_e(r)

    def test_corrupt_snapshot(self) -> None:
        tid, new = self.pre_ok("ls")
        for p in new:
            p.write_bytes(b"\x00not a snapshot{{{")
        self.assertEqual(self.status(), "", "the tree did not change; only e can block")
        self.assert_blocked_e(self.post(tid))

    def test_truncated_snapshot(self) -> None:
        tid, new = self.pre_ok("ls")
        for p in new:
            p.write_bytes(b"")
        self.assert_blocked_e(self.post(tid))

    def test_git_unavailable_at_post(self) -> None:
        tid, _ = self.pre_ok("ls")
        stub = self._git_stub()
        r = self.post(tid, env_override={"PATH": str(stub)})
        self.assert_blocked_e(r)

    def test_snapshot_keyed_by_session_and_tool_use_id(self) -> None:
        # Pre(A) on clean main; main gets dirty; Pre(B) sees the dirt.
        # Post(B) unchanged -> allow (its own snapshot); Post(A) -> block.
        tid_a, _ = self.pre_ok("ls")
        (self.main / "between.txt").write_text("b\n")
        tid_b, _ = self.pre_ok("ls")
        self.assert_allowed(self.post(tid_b))
        self.assert_blocked_d(self.post(tid_a), "between.txt")

    def test_other_session_same_tool_use_id_has_no_snapshot(self) -> None:
        tid, _ = self.pre_ok("ls")
        r = self.post(tid, session="sess-other-9999")
        self.assert_blocked_e(r)


class HeadMoveIntegrationOnly(ObserveGuardBase):
    """Ruling 2026-10-03 on case c: a HEAD move with a clean tree is allowed
    ONLY if the new HEAD is a merge commit (>=2 parents) OR is contained in some
    ref other than the branch main has checked out (a fast-forward to work that
    already lives on a worktree / feature branch). c2 = the existing
    test_completed_fast_forward_merge_clean, c3 = test_completed_non_ff_merge_commit_clean.
    """

    def _head(self) -> str:
        return git(self.main, "rev-parse", "HEAD").stdout.strip()

    def _only_main_contains_head(self) -> None:
        refs = git(self.main, "for-each-ref", "--contains", "HEAD",
                   "--format=%(refname)").stdout.split()
        self.assertEqual(refs, ["refs/heads/main"],
                         "anti-vacuity: the new HEAD must live on main alone")
        parents = git(self.main, "rev-list", "--parents", "-n1", "HEAD").stdout.split()
        self.assertLessEqual(len(parents) - 1, 1, "anti-vacuity: new HEAD is not a merge")

    def test_c1_direct_commit_am_on_main_dirty_before_pre_clean_after(self) -> None:
        (self.main / "tracked.txt").write_text("edited before the command\n")
        tid, _ = self.pre_ok("git commit -am direct")
        old = self._head()
        git(self.main, "commit", "-q", "-am", "direct commit on main")
        self.assertNotEqual(old, self._head())
        self.assertEqual(self.status(), "", "anti-vacuity: tree is clean after")
        self._only_main_contains_head()
        self.assert_blocked_d(self.post(tid))

    def test_c1_direct_edit_and_commit_on_main_inside_one_command(self) -> None:
        tid, _ = self.pre_ok("echo x > tracked.txt && git commit -am direct")
        old = self._head()
        (self.main / "tracked.txt").write_text("edited and committed\n")
        git(self.main, "commit", "-q", "-am", "direct commit on main")
        self.assertNotEqual(old, self._head())
        self.assertEqual(self.status(), "")
        self._only_main_contains_head()
        self.assert_blocked_d(self.post(tid))

    def test_c4_amend_of_main_head_leaving_clean_tree(self) -> None:
        (self.main / "other.txt").write_text("o2\n")
        git(self.main, "commit", "-q", "-am", "a parented commit to amend")
        tid, _ = self.pre_ok("git commit --amend -m reworded")
        old = self._head()
        git(self.main, "commit", "-q", "--amend", "-m", "reworded on main")
        self.assertNotEqual(old, self._head(), "anti-vacuity: HEAD was rewritten")
        self.assertEqual(self.status(), "")
        self._only_main_contains_head()
        self.assert_blocked_d(self.post(tid))

    def test_c5_containment_or_parent_check_failing_at_post(self) -> None:
        """Same scenario as c2 (allowed when git works), but every git call that
        asks about ancestry / parents / ref containment fails. Status, HEAD,
        diff, ls-files and hash-object still work, so only that check fails.

        COUPLING NOTE: which argv counts as "the containment/parent check" is a
        list of spellings below. If the implementation uses a spelling outside
        the list, the stub never fires and the test FAILS with a message saying
        so (it does not pass vacuously)."""
        (self.wt / "new.txt").write_text("n\n")
        git(self.wt, "add", "new.txt")
        git(self.wt, "commit", "-q", "-m", "wt new file")
        tid, _ = self.pre_ok("git merge --ff-only wtbranch")
        git(self.main, "merge", "-q", "--ff-only", "wtbranch")
        self.assertEqual(self.status(), "")
        real_git = shutil.which("git")
        assert real_git
        d = self.tmp / "ancestrystub"
        d.mkdir()
        log = self.tmp / "denied.log"
        g = d / "git"
        g.write_text(
            "#!/bin/sh\n"
            "for a in \"$@\"; do\n"
            "  case \"$a\" in\n"
            "    --contains|--contains=*|--points-at*|--merged*|--no-merged*|"
            "for-each-ref|rev-list|merge-base|cat-file|branch|log|show|name-rev|"
            "--parents|*^@*|*^2*|*^{*|*~*)\n"
            f"      echo \"$*\" >> '{log}'\n"
            "      echo 'fatal: stub: ancestry query refused' >&2; exit 128;;\n"
            "  esac\n"
            "done\n"
            f"exec '{real_git}' \"$@\"\n")
        g.chmod(0o755)
        env = {"PATH": f"{d}{os.pathsep}{os.environ.get('PATH', '')}"}
        r = self.post(tid, env_override=env)
        self.assertTrue(log.exists() and log.read_text().strip(),
                        "the stub never refused a call: the implementation's "
                        "ancestry/containment query uses a spelling this test does "
                        f"not intercept (rc={r.returncode}, stderr={r.stderr!r})")
        self.assert_blocked_e(r)


class UnreadableDirtyPath(ObserveGuardBase):
    def test_dirty_path_unreadable_at_post_is_undetermined(self) -> None:
        if os.geteuid() == 0:
            self.skipTest("root ignores mode 000; the precondition cannot be built")
        f = self.main / "preexisting_untracked.txt"
        f.write_text("u1\n")
        (self.main / "tracked.txt").write_text("dirty before pre\n")
        tid, _ = self.pre_ok("chmod 000 preexisting_untracked.txt")
        f.chmod(0)
        self.addCleanup(f.chmod, 0o644)
        with self.assertRaises(PermissionError, msg="anti-vacuity: really unreadable"):
            f.read_bytes()
        r = self.post(tid)
        self.assert_blocked_e(r)


class FailedCommandAndEventNames(ObserveGuardBase):
    """A failing Bash call is followed by PostToolUseFailure, not PostToolUse
    (observed 2026-10-03; tool_use_id identical across Pre and the failure
    event). It is judged exactly like PostToolUse. Any other event name on a
    Bash payload is refused."""

    FAIL = "PostToolUseFailure"

    def test_failure_event_after_write_into_main_blocks(self) -> None:
        cmd = f"echo hi > {self.main}/x.txt; exit 1"
        tid, _ = self.pre_ok(cmd)
        r0 = subprocess.run(["sh", "-c", cmd])
        self.assertEqual(r0.returncode, 1, "anti-vacuity: the command really failed")
        self.assertTrue((self.main / "x.txt").exists(), "anti-vacuity")
        self.assert_blocked_d(self.run_hook(self.FAIL, tid, cmd), "x.txt")

    def test_failure_event_after_staging_in_main_blocks(self) -> None:
        (self.main / "staged.txt").write_text("s\n")
        tid, _ = self.pre_ok("git add staged.txt && false")
        git(self.main, "add", "staged.txt")
        self.assert_blocked_d(self.run_hook(self.FAIL, tid), "staged.txt")

    def test_failure_event_without_change_allows_and_consumes_snapshot(self) -> None:
        tid, new = self.pre_ok("false")
        self.assertEqual(subprocess.run(["false"]).returncode, 1)
        self.assert_allowed(self.run_hook(self.FAIL, tid, "false"))
        self.assertFalse(new & self.state_files(),
                         "the snapshot must be consumed by PostToolUseFailure")
        # Consumed means a second Post for the same id has nothing to compare.
        self.assert_blocked_e(self.post(tid, "false"))

    def test_failure_event_after_worktree_only_write_allows(self) -> None:
        tid, _ = self.pre_ok("touch f; false", cwd=self.wt)
        (self.wt / "f").write_text("w\n")
        self.assert_allowed(self.run_hook(self.FAIL, tid, cwd=self.wt))

    def test_failure_event_without_pre_snapshot_is_undetermined(self) -> None:
        self.assert_blocked_e(self.run_hook(self.FAIL, "toolu_never_pre_fail"))

    def test_failure_event_with_corrupt_snapshot_is_undetermined(self) -> None:
        tid, new = self.pre_ok("false")
        for p in new:
            p.write_bytes(b"{garbage")
        self.assert_blocked_e(self.run_hook(self.FAIL, tid))

    def test_unknown_event_names_on_bash_payload_refuse(self) -> None:
        for ev in ("PermissionRequest", "Stop", "posttooluse", "PostToolUseFailed",
                   "", None):
            with self.subTest(event=ev):
                # After a real Pre with no change, so only the name can refuse.
                tid, _ = self.pre_ok("ls")
                r = self.run_hook(ev, tid, "ls")
                self.assertEqual(r.returncode, 2,
                                 f"event {ev!r} must be refused; stderr={r.stderr!r}")
                self.assertTrue(r.stderr.strip())

    def test_unknown_event_name_without_pre_refuses(self) -> None:
        r = self.run_hook("Notification", "toolu_no_pre_unknown")
        self.assertEqual(r.returncode, 2, r.stderr)


class OrphanSnapshotSweep(ObserveGuardBase):
    """A call denied by another PreToolUse hook gets no Post event, so its
    snapshot is orphaned. Contract: PreToolUse removes snapshot files older
    than 24h in <common>/maintree-guard/ and never removes younger ones.

    The planted snapshots are made by the guard itself (a real Pre with
    another tool_use_id), so the test does not depend on the file naming."""

    def _plant(self) -> set[Path]:
        _tid, files = self.pre_ok("ls")
        return files

    @staticmethod
    def _backdate(files: set[Path], age_hours: float) -> None:
        t = __import__("time").time() - age_hours * 3600
        for f in files:
            os.utime(f, (t, t))

    def _orphan(self, age_hours: float) -> set[Path]:
        files = self._plant()
        self._backdate(files, age_hours)
        return files

    def test_pre_sweeps_old_orphans_and_keeps_young_ones(self) -> None:
        # Plant all three with real Pres FIRST (each Pre may sweep), and only
        # then backdate: backdating runs no Pre, so nothing is swept before the
        # existence check below.
        old = self._plant()
        young = self._plant()
        fresh = self._plant()
        self._backdate(old, 25)
        self._backdate(young, 23)
        self.assertTrue(old and young and fresh)
        self.assertTrue(all(f.exists() for f in old | young | fresh), "anti-vacuity")
        self.pre_ok("ls")
        self.assertFalse(any(f.exists() for f in old),
                         "a snapshot older than 24h must be removed at Pre")
        self.assertTrue(all(f.exists() for f in young),
                        "a 23h-old snapshot (another in-flight call's) must survive")
        self.assertTrue(all(f.exists() for f in fresh),
                        "a fresh snapshot must survive")

    def test_young_snapshot_survives_sweep_and_is_still_usable(self) -> None:
        tid_young, _ = self.pre_ok("ls")
        self._orphan(25)
        self.pre_ok("ls")  # triggers the sweep
        self.assert_allowed(self.post(tid_young))


BS = "\\"
BQ = "`"
Q = "'"


class RouteIndependence(ObserveGuardBase):
    """Ported from the static guard's spelling tables (ae4543d5 + verify1-4,
    test_maintree_isolation_guards, s05/s06 BashGuard, 29b08fc7, 06baf54d,
    55826e4f, ee273b5e, 214bb9d4, f036e218, b5358f58 incl. its main-side
    test_guard_maintree_bash_b5358f58*.py, ad524af9, 1a73a49b).

    The old tests asked "is this SPELLING refused at Pre?". The property that
    survives the switch to observation is route independence: whatever spelling
    is used, a command that REALLY changes main is reported at Post, and one
    that really only changes a worktree / scratch dir (or nothing) is not.
    Every row RUNS the command for real between Pre and Post:
      * main-writing rows assert main's fingerprint (status + content + mode of
        every non-ignored file) really changed, then expect case d;
      * allow rows assert their own proof of effect (the target was written /
        the read produced output) and that main's fingerprint did NOT change.
    A row whose interpreter is not installed is skipped by name, not passed.
    """

    def setUp(self) -> None:
        super().setUp()
        self.scratch = self.tmp / "scratch"
        self.home = self.tmp / "home"
        self.home.mkdir()

    # -- fixture plumbing ---------------------------------------------------
    def _reset(self) -> None:
        for tree in (self.main, self.wt):
            git(tree, "reset", "-q", "--hard")
            git(tree, "clean", "-fdq")
        shutil.rmtree(self.scratch, ignore_errors=True)
        (self.scratch / "src").mkdir(parents=True)
        (self.scratch / "src" / "p.txt").write_text("x\n")
        (self.scratch / "tmpd").mkdir()
        (self.scratch / "junk").mkdir()

    def _fingerprint(self) -> tuple:
        st = git(self.main, "status", "--porcelain=v2", "--untracked-files=all").stdout
        files = git(self.main, "ls-files", "-z", "-co", "--exclude-standard").stdout
        h = []
        for rel in sorted(p for p in files.split("\0") if p):
            f = self.main / rel
            if f.is_symlink():
                h.append((rel, "link:" + os.readlink(f)))
            elif f.is_file():
                h.append((rel, hashlib.sha256(f.read_bytes()).hexdigest(),
                          os.stat(f).st_mode & 0o777))
            else:
                h.append((rel, None))
        return st, tuple(h)

    def _bash_env(self, extra: dict | None = None) -> dict:
        env = _clean_env()
        env["HOME"] = str(self.home)
        env["TMPDIR"] = str(self.scratch / "tmpd")
        env.pop("PWD", None)
        if extra:
            env.update(extra)
        return env

    def _cycle(self, cmd: str, cwd: Path, env_extra: dict | None = None):
        tid, _ = self.pre_ok(cmd, cwd=cwd)
        r = subprocess.run(["bash", "-c", cmd], cwd=cwd, env=self._bash_env(env_extra),
                           capture_output=True, text=True, timeout=60)
        event = "PostToolUse" if r.returncode == 0 else "PostToolUseFailure"
        return r, self.run_hook(event, tid, cmd, cwd=cwd)

    def _missing(self, tools) -> str | None:
        for t in tools:
            if shutil.which(t) is None:
                return t
        return None

    # -- tables ----------------------------------------------------------------
    def _main_writes(self):
        M, W, S, T = self.main, self.wt, self.scratch, self.tmp
        rows = [
            # (name, command, cwd, required tools, extra env)
            ("redirect", f"echo hi > {M}/p.txt", W, (), None),
            ("append-tracked", f"echo hi >> {M}/tracked.txt", W, (), None),
            ("truncate-colon", f": > {M}/tracked.txt", W, (), None),
            ("heredoc-into-main", f"cat > {M}/note.txt <<'EOF'\nit's fine\nEOF", W, (), None),
            ("tee", f"echo x | tee {M}/t.txt >/dev/null", W, (), None),
            ("dd", f"echo x | dd of={M}/dd.txt 2>/dev/null", W, (), None),
            ("cp-wt-to-main", f"cp {W}/tracked.txt {M}/g.txt", W, (), None),
            ("mv-in-main", f"mv {M}/other.txt {M}/moved.txt", W, (), None),
            ("rm", f"rm {M}/tracked.txt", W, (), None),
            ("rm-glob", f"rm -f {M}/*.txt", W, (), None),
            ("rm-glob-from-ancestor", f"rm -f {T}/*/other.txt", W, (), None),
            ("touch", f"touch {M}/zz", W, (), None),
            ("mkdir-then-file", f"mkdir -p {M}/d && touch {M}/d/f", W, (), None),
            ("ln-s", f"ln -s /etc/hosts {M}/link", W, (), None),
            ("chmod-tracked", f"chmod +x {M}/tracked.txt", W, (), None),
            ("sed-i-suffix", f"sed -i.bak s/v1/v2/ {M}/tracked.txt", W, (), None),
            ("perl-pi", f"perl -pi -e 's/v1/v2/' {M}/tracked.txt", W, ("perl",), None),
            ("perl-hex-escaped-slash-unlink",
             Q.join(["perl -e ", f'unlink "{BS}x2f{str(M)[1:]}/tracked.txt"', ""]), W, ("perl",), None),
            ("perl-octal-escaped-slash-unlink",
             Q.join(["perl -e ", f'unlink "{BS}057{str(M)[1:]}/tracked.txt"', ""]), W, ("perl",), None),
            ("ruby-pi", f"ruby -pi -e 'gsub(/v1/,\"v2\")' {M}/tracked.txt", W, ("ruby",), None),
            ("python-open-write", f"python3 -c \"open('{M}/py.txt','w').write('x')\"", W, (), None),
            ("python-os-remove", f"python3 -c \"import os; os.remove('{M}/tracked.txt')\"", W, (), None),
            ("python-pathlib",
             f"python3 -c \"import pathlib; pathlib.Path('{M}/pl.txt').write_text('x')\"", W, (), None),
            ("python-shutil-copy",
             f"python3 -c \"import shutil; shutil.copy('{W}/tracked.txt', '{M}/sc.txt')\"", W, (), None),
            ("python-subprocess",
             f"python3 -c \"import subprocess; subprocess.run(['touch', '{M}/sp.txt'])\"", W, (), None),
            ("python-heredoc", f"python3 - <<'PY'\nopen('{M}/h.txt','w').write('x')\nPY", W, (), None),
            ("python-argv-relative", "python3 -c \"import sys; open(sys.argv[1],'w')\" rel.txt", M, (), None),
            ("node-writeFileSync", f"node -e \"require('fs').writeFileSync('{M}/n.txt','x')\"", W,
             ("node",), None),
            ("awk-redirect", f"awk 'BEGIN{{print \"x\" > \"{M}/a.txt\"}}'", W, ("awk",), None),
            ("sh-c", f"sh -c 'echo hi > {M}/p.txt'", W, (), None),
            ("bash-lc", f"bash -lc 'echo hi > {M}/p.txt'", W, (), None),
            ("eval", f"eval 'echo hi > {M}/p.txt'", W, (), None),
            ("env-prefix", f"env FOO=1 rm {M}/tracked.txt", W, (), None),
            ("nohup", f"nohup touch {M}/nh.txt >/dev/null 2>&1", W, (), None),
            ("xargs", f"echo {M}/x.txt | xargs touch", W, (), None),
            ("find-exec-embedded-braces", f"find {M} -maxdepth 0 -exec touch {{}}/p.txt {BS};", W, (), None),
            ("command-substitution", f"echo $(touch {M}/s.txt)", W, (), None),
            ("nested-escaped-backquote", f"echo {BQ}echo {BS}{BQ}touch {M}/n.txt{BS}{BQ}{BQ}", W, (), None),
            ("cmd-from-substitution", f"CMD=$(echo touch {M}/c.txt); $CMD", W, (), None),
            ("variable-path", f"WT={M}; sed -i.bak s/v1/v2/ $WT/tracked.txt", W, (), None),
            ("tilde", "touch ~/tilde.txt", W, (), {"HOME": str(M)}),
            ("newline-second-line", f"echo hi\nrm {M}/tracked.txt", W, (), None),
            ("newline-first-line", f"rm {M}/tracked.txt\necho hi", W, (), None),
            ("cd-elsewhere-then-absolute-main", f"cd {S} && rm {M}/tracked.txt", W, (), None),
            ("cd-main-then-relative", f"cd {M} && echo hi > rel.txt", W, (), None),
            ("subshell-cd-then-relative-in-main-cwd", "(cd /tmp) && echo hi > f.txt", M, (), None),
            ("relative-sed-in-main-cwd", "sed -i.bak s/v1/v2/ tracked.txt", M, (), None),
            ("mktemp-bare-template", f"mktemp {M}/x.XXXX", W, (), None),
            ("mktemp-p-dir", f"mktemp -p {M}", W, (), None),
            ("mktemp-p-template-escapes-its-dir", f"mktemp -p {S} ../main/x.XXXX", W, (), None),
            ("rsync-trailing-option", f"rsync -a {S}/src/ {M}/ --exclude zz", W, ("rsync",), None),
            ("tar-extract", f"tar -cf {S}/a.tar -C {S}/src p.txt && tar -xf {S}/a.tar -C {M}", W,
             ("tar",), None),
            ("git-C-add", f"echo n > {M}/ga.txt && git -C {M} add ga.txt", W, (), None),
            # b5358f58 (cp/mv/ln/install/rsync judged by what they WRITE), ported
            # from test_guard_maintree_bash_b5358f58*.py: each row really writes main.
            ("cp-several-sources-into-main-dir", f"cp /etc/hosts {S}/src/p.txt {M}/", W, (), None),
            ("install-into-main", f"install -m 644 {S}/src/p.txt {M}/inst.txt", W, ("install",), None),
            ("mv-out-of-main-removes-its-source", f"mv {M}/other.txt {W}/moved.txt", W, (), None),
            ("rsync-remove-source-files-from-main",
             f"rsync -a --remove-source-files {M}/other.txt {S}/", W, ("rsync",), None),
            ("hard-link-into-main", f"ln {S}/src/p.txt {M}/hl.txt", W, (), None),
            ("write-through-hard-link-of-main-file", f"ln {M}/tracked.txt {S}/hl && echo x >> {S}/hl",
             W, (), None),
            ("cp-through-wt-symlink-to-main-dir", f"ln -s {M} {W}/tomain && cp {S}/src/p.txt {W}/tomain/",
             W, (), None),
            ("lone-operand-ln-s-lands-in-main-cwd", "ln -s /etc/hosts", M, (), None),
            ("bracket-glob-naming-main", f"cp /etc/hosts {T}/[m]ain/", W, (), None),
        ]
        if sys.platform != "darwin":
            # GNU mktemp honours TMPDIR; macOS BSD mktemp ignored it here
            # (observed 2026-10-03: it used DARWIN_USER_TEMP_DIR), so on darwin
            # the row would be a fixture that changes nothing.
            rows.append(("mktemp-TMPDIR-main", "mktemp", W, (), {"TMPDIR": str(M)}))
        if sys.platform == "darwin":
            r = subprocess.run(["getconf", "DARWIN_USER_TEMP_DIR"], capture_output=True, text=True)
            ut = os.path.realpath(r.stdout.strip()) if r.returncode == 0 and r.stdout.strip() else None
            if ut:
                rows.append(("mktemp-t-prefix-escapes-tmpdir",
                             f"mktemp -t {os.path.relpath(str(M), ut)}/x", W, (), {"TMPDIR": ut}))
        return rows

    def test_every_route_that_really_writes_main_is_reported(self) -> None:
        for name, cmd, cwd, tools, env_extra in self._main_writes():
            with self.subTest(route=name):
                miss = self._missing(tools)
                if miss:
                    self.skipTest(f"{name}: {miss} not installed")
                self._reset()
                before = self._fingerprint()
                r, post = self._cycle(cmd, cwd, env_extra)
                self.assertNotEqual(before, self._fingerprint(),
                                    f"anti-vacuity: {name} did not change main "
                                    f"(rc={r.returncode} stderr={r.stderr[:200]!r})")
                self.assert_blocked_d(post)

    def _non_main_effects(self):
        M, W, S = self.main, self.wt, self.scratch

        def exists(p):
            return lambda r: p.exists()

        def gone(p):
            return lambda r: not p.exists()

        def says(s):
            return lambda r: r.returncode == 0 and s in r.stdout

        def wt_v2(r):
            return (W / "tracked.txt").read_text() == "v2\n"

        commit = (f"echo c > {W}/c.txt && git -C {W} add c.txt && git -C {W} -c user.name=t "
                  f"-c user.email=t@e -c commit.gpgsign=false -c core.hooksPath=/dev/null "
                  f"commit -q -m 'x > y'")
        return [
            ("redirect-into-wt", f"echo hi > {W}/p.txt", M, (), exists(W / "p.txt")),
            ("sed-wt-absolute", f"sed -i.bak s/v1/v2/ {W}/tracked.txt", M, (), wt_v2),
            ("cd-wt-sed-relative", f"cd {W} && sed -i.bak s/v1/v2/ tracked.txt", M, (), wt_v2),
            ("relative-redirect-in-wt-cwd", "echo hi > out.txt", W, (), exists(W / "out.txt")),
            ("relative-sed-in-wt-cwd", "sed -i.bak s/v1/v2/ tracked.txt", W, (), wt_v2),
            ("perl-pi-wt", f"perl -pi -e 's/v1/v2/' {W}/tracked.txt", M, ("perl",), wt_v2),
            ("python-write-wt", f"python3 -c \"open('{W}/p.txt','w').write('x')\"", M, (),
             exists(W / "p.txt")),
            ("bash-c-wt", f"bash -c 'echo hi > {W}/p.txt'", M, (), exists(W / "p.txt")),
            ("rm-glob-wt", f"rm -f {W}/*.txt", M, (), gone(W / "tracked.txt")),
            ("newline-wt-only", f"echo hi\nrm {W}/tracked.txt", M, (), gone(W / "tracked.txt")),
            ("newline-benign-second-line", f"cp {W}/tracked.txt {W}/copy.rs\nls {M}", M, (),
             exists(W / "copy.rs")),
            ("git-commit-in-wt", commit, M, (),
             lambda r: r.returncode == 0 and (W / "c.txt").exists()),
            ("heredoc-body-names-main-but-writes-wt",
             f"cat > {W}/note.txt <<'EOF'\nrm {M}/tracked.txt\nit's fine\nEOF", M, (),
             exists(W / "note.txt")),
            ("cp-reading-main-into-wt", f"cp {M}/tracked.txt {W}/f.copy", M, (), exists(W / "f.copy")),
            ("cp-into-scratch", f"cp /etc/hosts {S}/h", W, (), exists(S / "h")),
            ("cp-into-scratch-fd-redirect", f"cp /etc/hosts {S}/h 2>/dev/null", W, (), exists(S / "h")),
            ("mkdir-scratch", f"mkdir {S}/newdir", W, (), exists(S / "newdir")),
            ("rm-scratch-variable", f"D={S}/junk; rm -rf $D", W, (), gone(S / "junk")),
            ("same-line-variables-outside-main", f"A={S}/src; B={S}; cp $A/p.txt $B/b.txt", W, (),
             exists(S / "b.txt")),
            ("assignment-then-redirect-into-scratch", f"X={S}\necho hi > $X/r.txt", W, (),
             exists(S / "r.txt")),
            ("cd-main-git-show-into-scratch", f"cd {M} && git show HEAD:tracked.txt > {S}/shown", W, (),
             lambda r: (S / "shown").exists() and (S / "shown").read_text() == "v1\n"),
            ("tilde-outside-main", "touch ~/scratch.txt", M, (),
             lambda r: (self.home / "scratch.txt").exists()),
            ("mktemp-outside-main", "mktemp", W, (),
             self._mktemp_outside_main),
            ("git-worktree-add-from-main", f"git -C {M} worktree add -q -b nb {self.tmp}/nb HEAD", M, (),
             exists(self.tmp / "nb" / "tracked.txt")),
            ("read-cat-main", f"cat {M}/tracked.txt", W, (), says("v1")),
            ("read-python-main", f"python3 -c \"print(open('{M}/tracked.txt').read())\"", W, (),
             says("v1")),
            ("read-awk-main", f"awk '{{print $1}}' {M}/tracked.txt", W, ("awk",), says("v1")),
            ("read-node-main",
             f"node -e \"console.log(require('fs').readFileSync('{M}/tracked.txt','utf8'))\"", W,
             ("node",), says("v1")),
            ("unparseable-command-runs-nothing", f"rm -rf {M}/'tracked.txt", M, (),
             lambda r: r.returncode != 0 and (M / "tracked.txt").exists()),
            # b5358f58, ported from test_guard_maintree_bash_b5358f58*.py: reading
            # main (as a cp / install / ln / rsync SOURCE) while writing elsewhere.
            ("cp-relative-source-in-main-cwd-into-wt", f"cp tracked.txt {W}/f.copy", M, (),
             exists(W / "f.copy")),
            ("cp-several-main-sources-into-wt-dir",
             f"mkdir {W}/sub && cp {M}/tracked.txt {M}/other.txt {W}/sub/", M, (),
             exists(W / "sub" / "other.txt")),
            ("install-main-into-wt", f"install -m 644 {M}/tracked.txt {W}/inst.txt", M, ("install",),
             exists(W / "inst.txt")),
            ("symlink-in-wt-pointing-at-main", f"ln -s {M}/tracked.txt {W}/lnk", M, (),
             lambda r: (W / "lnk").is_symlink()),
            ("hard-link-of-main-file-into-scratch", f"ln {M}/tracked.txt {S}/hl", W, (),
             exists(S / "hl")),
            ("rsync-copy-out-of-main", f"rsync -a {M}/tracked.txt {S}/r.txt", W, ("rsync",),
             exists(S / "r.txt")),
        ]

    def _mktemp_outside_main(self, r) -> bool:
        made = Path(r.stdout.strip())
        ok = (r.returncode == 0 and made.is_file()
              and not os.path.realpath(made).startswith(os.path.realpath(self.main) + os.sep))
        if made.is_file():
            made.unlink()  # on macOS it lands in the per-user temp dir, outside the fixture
        return ok

    def test_routes_that_do_not_change_main_are_allowed(self) -> None:
        for name, cmd, cwd, tools, proof in self._non_main_effects():
            with self.subTest(route=name):
                miss = self._missing(tools)
                if miss:
                    self.skipTest(f"{name}: {miss} not installed")
                self._reset()
                shutil.rmtree(self.tmp / "nb", ignore_errors=True)
                git(self.main, "worktree", "prune")
                git(self.main, "branch", "-q", "-D", "nb", check=False)
                before = self._fingerprint()
                r, post = self._cycle(cmd, cwd)
                self.assertTrue(proof(r), f"anti-vacuity: {name} had no observable effect "
                                          f"(rc={r.returncode} stdout={r.stdout[:120]!r} "
                                          f"stderr={r.stderr[:200]!r})")
                self.assertEqual(before, self._fingerprint(), f"{name} changed main")
                self.assert_allowed(post)

    def test_1a73a49b_git_checkout_reverting_dirty_main_file_is_reported(self) -> None:
        """backlog 1a73a49b: `git -C <main> checkout -- <file>` discarding a
        dirty main file was rc 0 under the static guard. It changes main's
        dirty state outside integration (no MERGE_HEAD, HEAD unmoved), so the
        observing guard reports it (case d) whatever cwd it is spelled from."""
        for cwd_name in ("wt", "main"):
            with self.subTest(cwd=cwd_name):
                cwd = self.wt if cwd_name == "wt" else self.main
                self._reset()
                (self.main / "tracked.txt").write_text("dirty\n")
                before = self._fingerprint()
                r, post = self._cycle(f"git -C {self.main} checkout -- tracked.txt", cwd)
                self.assertEqual(r.returncode, 0, r.stderr)
                self.assertEqual((self.main / "tracked.txt").read_text(), "v1\n",
                                 "anti-vacuity: checkout did not revert the dirty file")
                self.assertNotEqual(before, self._fingerprint())
                self.assert_blocked_d(post, "tracked.txt")

    def test_1a73a49b_control_git_checkout_on_clean_main_file_is_allowed(self) -> None:
        """Control: the same spelling on an already-clean file changes nothing."""
        self._reset()
        before = self._fingerprint()
        r, post = self._cycle(f"git -C {self.main} checkout -- tracked.txt", self.wt)
        self.assertEqual(r.returncode, 0, r.stderr)
        self.assertEqual(before, self._fingerprint())
        self.assert_allowed(post)


class MergeTimePorts(ObserveGuardBase):
    def _conflict(self) -> None:
        git(self.main, "checkout", "-q", "-b", "side")
        (self.main / "tracked.txt").write_text("side\n")
        git(self.main, "commit", "-q", "-am", "side")
        git(self.main, "checkout", "-q", "main")
        (self.main / "tracked.txt").write_text("mainline\n")
        git(self.main, "commit", "-q", "-am", "mainline")
        git(self.main, "merge", "side", check=False)
        self.assertTrue((self.main / ".git" / "MERGE_HEAD").exists(), "anti-vacuity")

    def test_ee273b5e_cp_resolved_file_into_conflicted_path_mid_merge(self) -> None:
        # Port of test_backlog_ee273b5e: resolving a conflict by copying the
        # resolved content from a worktree is integration (case b).
        self._conflict()
        (self.wt / "resolved.txt").write_text("resolved\n")
        cmd = f"cp {self.wt}/resolved.txt {self.main}/tracked.txt"
        tid, _ = self.pre_ok(cmd)
        subprocess.run(["sh", "-c", cmd], check=True)
        self.assertEqual((self.main / "tracked.txt").read_text(), "resolved\n")
        self.assert_allowed(self.post(tid, cmd))

    def test_ee273b5e_control_cp_into_main_outside_merge_blocks(self) -> None:
        cmd = f"cp {self.wt}/tracked.txt {self.main}/copied.txt"
        tid, _ = self.pre_ok(cmd)
        subprocess.run(["sh", "-c", cmd], check=True)
        self.assertTrue((self.main / "copied.txt").exists())
        self.assert_blocked_d(self.post(tid, cmd), "copied.txt")

    def test_d796a630_untracked_merge_blocker_can_be_removed_then_merged(self) -> None:
        # Port of s05 d796a630: the gates must not form a cycle. Under
        # observation the rm is never prevented (Pre allows), so the merge that
        # the untracked file was blocking can proceed. The removal itself is a
        # change of main outside integration and is REPORTED (case d); the
        # report does not undo it.
        git(self.wt, "checkout", "-q", "-b", "feat2")
        (self.wt / "docs").mkdir()
        (self.wt / "docs" / "spec.md").write_text("spec\n")
        git(self.wt, "add", "docs")
        git(self.wt, "commit", "-q", "-m", "spec")
        (self.main / "docs").mkdir()
        (self.main / "docs" / "spec.md").write_text("spec\n")
        blocked = git(self.main, "merge", "feat2", check=False)
        self.assertNotEqual(blocked.returncode, 0, "anti-vacuity: untracked file blocks the merge")
        cmd = f"rm {self.main}/docs/spec.md"
        tid, _ = self.pre_ok(cmd)
        subprocess.run(["sh", "-c", cmd], check=True)
        self.assert_blocked_d(self.post(tid, cmd), "spec.md")
        merged = git(self.main, "merge", "-q", "feat2", check=False)
        self.assertEqual(merged.returncode, 0, merged.stderr)


class BadPayloads(ObserveGuardBase):
    """Port of test_maintree_isolation_guards' 02bdd012 payload rows."""

    def _raw(self, raw: str) -> subprocess.CompletedProcess:
        env = _clean_env()
        env["CLAUDE_PROJECT_DIR"] = str(self.main)
        return subprocess.run([sys.executable, str(SCRIPT)], input=raw, cwd=str(self.main),
                              env=env, capture_output=True, text=True, timeout=60)

    def test_unreadable_payloads_refuse(self) -> None:
        for raw in ("this is not json {", "", "[1, 2, 3]", '"rm -rf x"', "null", "42"):
            with self.subTest(raw=raw):
                r = self._raw(raw)
                self.assertEqual(r.returncode, 2, r.stderr)
                self.assertTrue(r.stderr.strip())
        self.assertEqual(self.state_files(), set(), "a refused payload writes no state")

    def test_bash_payload_without_snapshot_keys_refuses(self) -> None:
        base = {"hook_event_name": "PreToolUse", "session_id": "s", "tool_use_id": "t",
                "tool_name": "Bash", "tool_input": {"command": "ls"}, "cwd": str(self.main)}
        r = self._raw(json.dumps(base))
        self.assertEqual(r.returncode, 0, f"control: the complete payload is allowed: {r.stderr}")
        for key in ("session_id", "tool_use_id"):
            with self.subTest(missing=key):
                p = dict(base)
                del p[key]
                r = self._raw(json.dumps(p))
                self.assertEqual(r.returncode, 2, f"{key} missing: cannot key the snapshot")


class NonBashTool(ObserveGuardBase):
    def test_non_bash_pre_and_post_allow_without_state(self) -> None:
        tid = self._next_id()
        r1 = self.run_hook("PreToolUse", tid, tool_name="Edit")
        self.assertEqual(r1.returncode, 0, r1.stderr)
        self.assertEqual(self.state_files(), set(), "no state for non-Bash tools")
        (self.main / "edited.txt").write_text("e\n")
        r2 = self.run_hook("PostToolUse", tid, tool_name="Edit")
        self.assertEqual(r2.returncode, 0, r2.stderr)
        self.assertEqual(self.state_files(), set())


if __name__ == "__main__":
    unittest.main()
