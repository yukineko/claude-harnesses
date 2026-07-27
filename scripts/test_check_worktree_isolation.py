#!/usr/bin/env python3
"""Regression coverage for the MERGE_HEAD race in check-worktree-isolation.py.

Observed live (backlog, three repetitions): a legitimate `git merge --no-edit
<branch>` in the main working tree was BLOCKED by check-worktree-isolation.py
with "Not committing merge; use git commit to complete the merge.", even though
the merge was proceeding cleanly. Immediately after, `MERGE_HEAD` was confirmed
to exist, and re-running `python3 scripts/check-worktree-isolation.py` standalone
returned exit 0 (allow). So the fact pattern is: MERGE_HEAD IS being written for
this merge, but at the instant `.githooks/pre-merge-commit` -> pre-commit ->
this script ran, the file was not yet visible — a filesystem-visibility race
between git writing MERGE_HEAD and the hook chain reading for it.

This is the SAME class of gap `crates/condukt/src/maintree.rs::declared_integration`
was written to close for the Rust `guard main-tree` gate (see
`crates/condukt/tests/main_tree_guard_merge.rs`): the on-disk marker is not
sufficient because git does not always write it (a `--no-ff` merge that succeeds
never writes MERGE_HEAD at all; a race can additionally delay visibility of one
that IS about to be written for a merge that stops). The corroborating signal is
the invocation context: `.githooks/pre-merge-commit` exports
`CONDUKT_GIT_HOOK=pre-merge-commit` before exec'ing `pre-commit`, and git itself
(not the caller) sets `GIT_REFLOG_ACTION=merge <ref>` (or `pull <ref>`) while
running that hook — confirmed here by an environment probe hook fired by a real
`git merge --no-ff`, mirrored in `test_env_signal_measured_from_a_real_merge`.

Both facts must be pinned:
  (1) RED: reproduced with a delayed-visibility MERGE_HEAD fixture, the
      PRE-FIX script blocks a merge it should allow (an on-disk-only check
      cannot distinguish "no merge" from "merge, but not yet visible").
  (2) GREEN: the same fixture, run against the actual current script, is
      allowed — via the invocation-context signal, which does not depend on
      filesystem timing at all, plus a small bounded retry on the on-disk
      marker as defense in depth (never an unbounded wait, never a blanket
      allow).

Existing safety properties (non-merge commit on main still blocks, worktree
commit still allows, non-repo fails closed, a FORGED CONDUKT_GIT_HOOK/
GIT_REFLOG_ACTION pair does not unlock an ordinary commit) are re-asserted here
too so a future edit cannot silently trade the fix for a wider bypass.
"""

from __future__ import annotations

import importlib.util
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest

SCRIPTS = os.path.dirname(os.path.abspath(__file__))
SCRIPT_PATH = os.path.join(SCRIPTS, "check-worktree-isolation.py")

_SPEC = importlib.util.spec_from_file_location("check_worktree_isolation", SCRIPT_PATH)
wti = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(wti)


def _run(cwd: str, env_extra=None, timeout: float = 15.0) -> subprocess.CompletedProcess:
    env = dict(os.environ)
    if env_extra:
        env.update(env_extra)
    return subprocess.run(
        [sys.executable, SCRIPT_PATH],
        cwd=cwd,
        capture_output=True,
        text=True,
        env=env,
        timeout=timeout,
    )


def _git(args, cwd, check=True):
    return subprocess.run(["git", *args], cwd=cwd, check=check,
                           capture_output=True, text=True)


class MergeHeadVisibilityRace(unittest.TestCase):
    """The fixture: MERGE_HEAD's appearance is delayed by a background thread,
    modelling the filesystem-visibility lag observed live. The script must not
    depend on winning that race."""

    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="wti-race-test.")
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.tmp]))
        self.main = os.path.join(self.tmp, "main")
        os.makedirs(self.main)
        _git(["init", "-q", "."], self.main)
        _git(["config", "user.email", "t@t"], self.main)
        _git(["config", "user.name", "t"], self.main)
        with open(os.path.join(self.main, "f"), "w") as fh:
            fh.write("hi\n")
        _git(["add", "-A"], self.main)
        _git(["commit", "-qm", "init"], self.main)
        self.git_dir = _git(
            ["rev-parse", "--absolute-git-dir"], self.main
        ).stdout.strip()

    def _delay_merge_head(self, delay_s: float):
        """Return (start_fn) that, once called, writes MERGE_HEAD only after
        `delay_s`, from a separate thread — simulating "git is about to make it
        visible, but not yet" rather than "no merge at all"."""
        path = os.path.join(self.git_dir, "MERGE_HEAD")

        def _writer():
            time.sleep(delay_s)
            with open(path, "w") as fh:
                fh.write("deadbeef\n")

        t = threading.Thread(target=_writer, daemon=True)
        return t, path

    # ---- RED: pin the bug in the pre-fix on-disk-only check ---------------
    def test_red_on_disk_only_check_blocks_during_the_visibility_gap(self):
        """Reproduces the reported race with the OLD (on-disk-only) logic
        directly, independent of whatever the current script does — this proves
        the fixture actually models the bug, and stays RED forever as a
        regression pin even after the script gains other signals."""
        t, path = self._delay_merge_head(delay_s=0.3)
        t.start()
        try:
            # This is the pre-fix decision procedure verbatim: MERGE_HEAD
            # existing on disk, checked once, with no other signal.
            old_style_allow = os.path.exists(path)
            self.assertFalse(
                old_style_allow,
                "the on-disk-only check must (incorrectly) NOT see the merge "
                "yet during the visibility gap -- this is the RED for the race",
            )
        finally:
            t.join(timeout=5)
        self.assertTrue(os.path.exists(path), "the writer must have landed it eventually")

    # ---- GREEN: the current script survives the same gap ------------------
    def test_green_script_allows_during_the_visibility_gap_via_invocation_context(self):
        """Same fixture. The FIXED script, invoked exactly as
        .githooks/pre-merge-commit invokes it (CONDUKT_GIT_HOOK set, and git's
        own GIT_REFLOG_ACTION corroborating a merge), must allow even though
        MERGE_HEAD is not yet visible -- the invocation-context signal does not
        depend on filesystem timing."""
        t, path = self._delay_merge_head(delay_s=0.3)
        t.start()
        try:
            result = _run(
                self.main,
                env_extra={
                    "CONDUKT_GIT_HOOK": "pre-merge-commit",
                    "GIT_REFLOG_ACTION": "merge feat",
                },
            )
        finally:
            t.join(timeout=5)
        self.assertEqual(
            result.returncode, 0,
            f"expected allow during the MERGE_HEAD visibility gap, got "
            f"rc={result.returncode} stderr={result.stderr!r}",
        )

    def test_green_script_allows_once_bounded_retry_catches_a_short_delay(self):
        """A SHORTER delay than the retry budget, with NO invocation-context
        signal at all (env vars absent) -- the bounded on-disk retry alone
        must be enough to catch it. This pins that the fix is not solely the
        invocation-context shortcut."""
        t, path = self._delay_merge_head(delay_s=0.05)
        t.start()
        try:
            result = _run(self.main, env_extra={})
        finally:
            t.join(timeout=5)
        self.assertEqual(
            result.returncode, 0,
            f"a short visibility delay must be absorbed by the bounded retry, "
            f"got rc={result.returncode} stderr={result.stderr!r}",
        )

    def test_retry_is_bounded_not_infinite(self):
        """If MERGE_HEAD never appears at all (no merge actually in progress),
        the script must still terminate promptly and BLOCK -- never wait
        forever, never fall back to allow just because it gave up looking."""
        started = time.monotonic()
        result = _run(self.main, env_extra={}, timeout=15.0)
        elapsed = time.monotonic() - started
        self.assertEqual(result.returncode, 1, "no merge signal at all must still block")
        self.assertLess(
            elapsed, 5.0,
            f"the bounded retry must not turn into a long/unbounded wait, took {elapsed:.2f}s",
        )


class DeclaredMergeIntegrationUnit(unittest.TestCase):
    """Direct unit coverage of the corroboration function, mirroring
    crates/condukt/src/maintree.rs::declared_integration's contract."""

    def test_both_signals_present_is_true(self):
        self.assertTrue(
            wti.declared_merge_integration(
                {"CONDUKT_GIT_HOOK": "pre-merge-commit", "GIT_REFLOG_ACTION": "merge feat"}
            )
        )

    def test_pull_verb_is_also_honored(self):
        self.assertTrue(
            wti.declared_merge_integration(
                {"CONDUKT_GIT_HOOK": "pre-merge-commit", "GIT_REFLOG_ACTION": "pull origin main"}
            )
        )

    def test_hook_env_missing_is_false(self):
        self.assertFalse(
            wti.declared_merge_integration({"GIT_REFLOG_ACTION": "merge feat"})
        )

    def test_reflog_action_missing_is_false(self):
        # This is the forged-declaration case: CONDUKT_GIT_HOOK claimed by hand
        # on an ordinary commit, but git itself never set GIT_REFLOG_ACTION.
        self.assertFalse(
            wti.declared_merge_integration({"CONDUKT_GIT_HOOK": "pre-merge-commit"})
        )

    def test_wrong_hook_name_is_false(self):
        self.assertFalse(
            wti.declared_merge_integration(
                {"CONDUKT_GIT_HOOK": "pre-commit", "GIT_REFLOG_ACTION": "merge feat"}
            )
        )

    def test_unrelated_reflog_verb_is_false(self):
        self.assertFalse(
            wti.declared_merge_integration(
                {"CONDUKT_GIT_HOOK": "pre-merge-commit", "GIT_REFLOG_ACTION": "rebase feat"}
            )
        )

    def test_neither_signal_is_false(self):
        self.assertFalse(wti.declared_merge_integration({}))


class ExistingSafetyPropertiesStillHold(unittest.TestCase):
    """Re-run of the load-bearing behaviour from test_maintree_isolation_guards.py
    plus the forged-declaration case, so this file alone would catch the fix
    weakening the gate rather than fixing the race."""

    def setUp(self) -> None:
        self.tmp = tempfile.mkdtemp(prefix="wti-safety-test.")
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", self.tmp]))
        self.main = os.path.join(self.tmp, "main")
        os.makedirs(self.main)
        _git(["init", "-q", "."], self.main)
        _git(["config", "user.email", "t@t"], self.main)
        _git(["config", "user.name", "t"], self.main)
        with open(os.path.join(self.main, "tracked.rs"), "w") as fh:
            fh.write("hi\n")
        _git(["add", "-A"], self.main)
        _git(["commit", "-qm", "init"], self.main)
        self.wt = os.path.join(self.tmp, "wtA")
        _git(["worktree", "add", "-q", self.wt, "-b", "feat", "HEAD"], self.main)

    def test_nonmerge_commit_on_main_still_blocks(self):
        self.assertEqual(_run(self.main).returncode, 1)

    def test_worktree_commit_still_allows(self):
        self.assertEqual(_run(self.wt).returncode, 0)

    def test_real_merge_head_on_disk_still_allows(self):
        gd = _git(["rev-parse", "--absolute-git-dir"], self.main).stdout.strip()
        path = os.path.join(gd, "MERGE_HEAD")
        with open(path, "w") as fh:
            fh.write("x")
        try:
            self.assertEqual(_run(self.main).returncode, 0)
        finally:
            os.remove(path)

    def test_non_repo_still_fails_closed(self):
        self.assertEqual(_run(self.tmp).returncode, 1)

    def test_forged_hook_declaration_without_git_reflog_action_still_blocks(self):
        """A forged CONDUKT_GIT_HOOK on a plain commit, with GIT_REFLOG_ACTION
        explicitly cleared (as it would be for a real ordinary `git commit`),
        must not unlock the main tree. This is the load-bearing "not a blanket
        env-var escape hatch" property."""
        env = dict(os.environ)
        env["CONDUKT_GIT_HOOK"] = "pre-merge-commit"
        env.pop("GIT_REFLOG_ACTION", None)
        result = subprocess.run(
            [sys.executable, SCRIPT_PATH],
            cwd=self.main,
            capture_output=True,
            text=True,
            env=env,
        )
        self.assertEqual(
            result.returncode, 1,
            "CONDUKT_GIT_HOOK alone, uncorroborated by git's own "
            "GIT_REFLOG_ACTION, must not allow the commit",
        )


class EnvSignalMeasuredFromARealMerge(unittest.TestCase):
    """Confirms, with an actual `git merge --no-ff`, what environment variable
    git sets while running pre-merge-commit -- the fact the fix's
    invocation-context signal depends on. Uses a template dir (git itself
    copies it into .git/hooks via `git init --template`, not this test)."""

    def test_git_sets_reflog_action_merge_for_a_real_merge_commit(self):
        tmp = tempfile.mkdtemp(prefix="wti-env-probe.")
        self.addCleanup(lambda: subprocess.run(["rm", "-rf", tmp]))
        tmpl = os.path.join(tmp, "tmpl", "hooks")
        os.makedirs(tmpl)
        probe = os.path.join(tmpl, "pre-merge-commit")
        with open(probe, "w") as fh:
            fh.write(
                "#!/bin/sh\n"
                'echo "REFLOG=$GIT_REFLOG_ACTION" 1>&2\n'
                "exit 0\n"
            )
        os.chmod(probe, 0o755)

        repo = os.path.join(tmp, "repo")
        os.makedirs(repo)
        _git(["init", "-q", "."], repo)
        _git(["config", "user.email", "t@t"], repo)
        _git(["config", "user.name", "t"], repo)
        with open(os.path.join(repo, "f"), "w") as fh:
            fh.write("base\n")
        _git(["add", "-A"], repo)
        _git(["commit", "-qm", "init"], repo)
        _git(["checkout", "-qb", "feat"], repo)
        with open(os.path.join(repo, "g"), "w") as fh:
            fh.write("feat\n")
        _git(["add", "-A"], repo)
        _git(["commit", "-qm", "feat"], repo)
        _git(["checkout", "-q", "master"], repo, check=False)
        _git(["checkout", "-q", "main"], repo, check=False)
        with open(os.path.join(repo, "h"), "w") as fh:
            fh.write("main-side\n")
        _git(["add", "-A"], repo)
        _git(["commit", "-qm", "main-change"], repo)

        # Install the probe hook via git's own template copy (this is how
        # `.githooks/pre-merge-commit` gets installed by
        # `git config core.hooksPath`, but a template copy demonstrates the
        # SAME hook-firing mechanism without touching this repo's own hooks).
        subprocess.run(["git", "init", "-q", f"--template={os.path.join(tmp, 'tmpl')}"],
                        cwd=repo, check=True)

        merged = subprocess.run(
            ["git", "merge", "--no-edit", "feat"],
            cwd=repo, capture_output=True, text=True,
        )
        self.assertIn(
            "REFLOG=merge feat", merged.stderr + merged.stdout,
            f"expected git to set GIT_REFLOG_ACTION=merge <ref> while running "
            f"pre-merge-commit; got stdout={merged.stdout!r} stderr={merged.stderr!r}",
        )


if __name__ == "__main__":
    unittest.main()
