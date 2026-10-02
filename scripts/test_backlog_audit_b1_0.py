#!/usr/bin/env python3
"""Closure regression tests written by an independent verifier for the
backlog-closure audit (batch b1_0).

Each test class names the backlog id(s) it pins.

* Classes WITHOUT a skip prove a closure: observed GREEN at HEAD and RED with
  the fix construct removed.
* Classes guarded by ``AUDIT_RUN_OPEN_PINS`` pin a defect that is STILL OPEN
  under its surviving ticket. They are RED today; run them with
  ``AUDIT_RUN_OPEN_PINS=1`` and un-guard them in the commit that fixes the
  surviving ticket. They are skipped by default only so the ordinary suite
  reports the open defect as a skip with its id instead of a standing red.

Run from the repo root:  python3 -m unittest scripts.test_backlog_audit_b1_0
                    or:  python3 scripts/test_backlog_audit_b1_0.py
"""
from __future__ import annotations

import ast
import json
import os
import pathlib
import re
import shutil
import subprocess
import sys
import tempfile
import unittest

REPO = pathlib.Path(__file__).resolve().parent.parent
SCRIPTS = REPO / "scripts"
OPEN_PINS = os.environ.get("AUDIT_RUN_OPEN_PINS") == "1"
OPEN_PIN_REASON = "pins a still-open backlog item; set AUDIT_RUN_OPEN_PINS=1 to observe it RED"


def _git(cwd, *args, check=True):
    env = dict(
        os.environ,
        GIT_CONFIG_NOSYSTEM="1",
        GIT_AUTHOR_NAME="t",
        GIT_AUTHOR_EMAIL="t@t.t",
        GIT_COMMITTER_NAME="t",
        GIT_COMMITTER_EMAIL="t@t.t",
    )
    r = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True, env=env)
    if check and r.returncode != 0:
        raise AssertionError(f"git {args} failed in {cwd}: {r.stdout}{r.stderr}")
    return r


def _has_main_guard_calling_unittest_main(src: str) -> bool:
    """True iff the module has `if __name__ == "__main__":` whose body calls
    `unittest.main(...)` (any arguments)."""
    tree = ast.parse(src)
    for node in tree.body:
        if not isinstance(node, ast.If):
            continue
        t = node.test
        if not (
            isinstance(t, ast.Compare)
            and isinstance(t.left, ast.Name)
            and t.left.id == "__name__"
            and len(t.comparators) == 1
            and isinstance(t.comparators[0], ast.Constant)
            and t.comparators[0].value == "__main__"
        ):
            continue
        for sub in ast.walk(node):
            if (
                isinstance(sub, ast.Call)
                and isinstance(sub.func, ast.Attribute)
                and sub.func.attr == "main"
                and isinstance(sub.func.value, ast.Name)
                and sub.func.value.id == "unittest"
            ):
                return True
    return False


class Backlog06a9d33fDirectInvocationRunsTests(unittest.TestCase):
    """06a9d33f claimed 13 scripts/test_*.py run 0 tests and exit 0 when invoked
    as `python3 scripts/test_X.py` (no __main__ guard). The premise was a grep
    artifact (`grep -L "unittest.main()"` misses `unittest.main(verbosity=2)`).
    This pins the property the item cared about for EVERY suite: a direct run
    reaches unittest.main."""

    def test_every_scripts_suite_has_a_main_guard_calling_unittest_main(self):
        suites = sorted(SCRIPTS.glob("test_*.py"))
        self.assertGreater(len(suites), 10, "scope broke: too few scripts/test_*.py found")
        missing = [
            p.name
            for p in suites
            if not _has_main_guard_calling_unittest_main(p.read_text(encoding="utf-8"))
        ]
        self.assertEqual(
            missing,
            [],
            "06a9d33f: these suites run ZERO tests and exit 0 when invoked directly",
        )

    def test_a_direct_run_reports_a_nonzero_test_count(self):
        r = subprocess.run(
            [sys.executable, str(SCRIPTS / "test_check_launcher_exec_bit.py")],
            cwd=REPO,
            capture_output=True,
            text=True,
            timeout=300,
        )
        self.assertEqual(r.returncode, 0, r.stderr[-2000:])
        m = re.search(r"^Ran (\d+) tests?", r.stderr, re.M)
        self.assertIsNotNone(m, f"no 'Ran N tests' line:\n{r.stderr[-2000:]}")
        self.assertGreater(int(m.group(1)), 0)


class Backlog8cb3bc22LauncherExecBitGate(unittest.TestCase):
    """8cb3bc22: a gate must mechanically detect a plugin launcher recorded
    100644 in the git index (taintguard was found by hand), and it must be
    wired into the local pre-commit."""

    CHECKER = SCRIPTS / "check-launcher-exec-bit.py"

    def _repo(self, modes):
        d = pathlib.Path(tempfile.mkdtemp(prefix="audit-8cb3bc22-"))
        self.addCleanup(shutil.rmtree, d, True)
        _git(d, "init", "-q")
        _git(d, "config", "core.fileMode", "false")
        for name, mode in modes.items():
            p = d / "crates" / name / "bin" / name
            p.parent.mkdir(parents=True)
            p.write_text("#!/bin/sh\nexit 0\n")
            _git(d, "add", str(p.relative_to(d)))
            _git(d, "update-index", f"--chmod={'+x' if mode == '100755' else '-x'}",
                 str(p.relative_to(d)))
        return d

    def _run(self, cwd):
        return subprocess.run([sys.executable, str(self.CHECKER)], cwd=cwd,
                              capture_output=True, text=True, timeout=60)

    def test_a_100644_launcher_blocks_and_is_named(self):
        d = self._repo({"good": "100755", "bad": "100644"})
        r = self._run(d)
        self.assertEqual(r.returncode, 1, r.stdout + r.stderr)
        self.assertIn("crates/bad/bin/bad", r.stderr)
        self.assertNotIn("crates/good/bin/good:", r.stderr)

    def test_all_100755_passes(self):
        d = self._repo({"good": "100755", "also": "100755"})
        r = self._run(d)
        self.assertEqual(r.returncode, 0, r.stdout + r.stderr)

    def test_the_checker_is_wired_into_pre_commit(self):
        hook = (REPO / ".githooks" / "pre-commit").read_text(encoding="utf-8")
        live = [l for l in hook.splitlines()
                if "check-launcher-exec-bit.py" in l and not l.lstrip().startswith("#")]
        self.assertTrue(live, "8cb3bc22: .githooks/pre-commit never runs check-launcher-exec-bit.py")


def _fake_cargo_dir(body: str) -> pathlib.Path:
    d = pathlib.Path(tempfile.mkdtemp(prefix="audit-a0525604-cargo-"))
    p = d / "cargo"
    p.write_text("#!/bin/sh\n" + body)
    p.chmod(0o755)
    return d


def _run_rebuild_dry(fake_body: str):
    fake = _fake_cargo_dir(fake_body)
    cache = pathlib.Path(tempfile.mkdtemp(prefix="audit-a0525604-cache-"))
    try:
        env = dict(os.environ, PATH=f"{fake}:{os.environ.get('PATH', '')}",
                   CLAUDE_PLUGIN_CACHE=str(cache))
        return subprocess.run(["bash", str(SCRIPTS / "rebuild-plugins.sh"), "--dry-run"],
                              cwd=REPO, env=env, capture_output=True, text=True, timeout=120)
    finally:
        shutil.rmtree(fake, True)
        shutil.rmtree(cache, True)


class BacklogA0525604CargoMetadataFailureIsNotMasked(unittest.TestCase):
    """a0525604 (first half): a FAILED `cargo metadata` must not be masked by
    the pipeline into an empty TARGET_DIR and a green run."""

    def test_failed_cargo_metadata_aborts_nonzero(self):
        r = _run_rebuild_dry('echo "fake cargo: metadata failure" >&2\nexit 101\n')
        self.assertNotEqual(r.returncode, 0,
                            "a0525604: rebuild-plugins.sh exited 0 although cargo metadata "
                            f"failed:\n{r.stdout[-1500:]}{r.stderr[-1500:]}")
        self.assertNotIn("build dir:", r.stdout,
                         "a0525604: the run continued past a failed cargo metadata")


@unittest.skipUnless(OPEN_PINS, OPEN_PIN_REASON + " (a0525604, empty-metadata half)")
class OpenPinA0525604EmptyMetadataIsNotGreen(unittest.TestCase):
    """a0525604 (second half, STILL OPEN): `cargo metadata` that exits 0 but
    yields no target_directory silently falls back to $REPO/target."""

    def test_empty_metadata_does_not_fall_back_silently(self):
        r = _run_rebuild_dry("echo '{\"packages\":[]}'\nexit 0\n")
        self.assertNotEqual(r.returncode, 0,
                            "a0525604: empty cargo metadata fell back to $REPO/target and "
                            f"exited 0:\n{r.stdout[-1500:]}")


@unittest.skipUnless(OPEN_PINS, OPEN_PIN_REASON + " (840c4a17 == 233f819c)")
class OpenPin840c4a17StopVerifyWorktreeAttribution(unittest.TestCase):
    """840c4a17 == 233f819c (STILL OPEN): stop-verify-worktree.py blocks a
    session that edited nothing because ANOTHER session dirtied main, and its
    message prescribes moving "your" changes — it never states whose they are."""

    def test_block_message_states_attribution_of_foreign_dirt(self):
        d = pathlib.Path(tempfile.mkdtemp(prefix="audit-840c4a17-"))
        self.addCleanup(shutil.rmtree, d, True)
        _git(d, "init", "-q", "-b", "main")
        (d / "tasks.toml").write_text("a\n")
        _git(d, "add", "tasks.toml")
        _git(d, "commit", "-q", "-m", "seed")
        # Another session's uncommitted write; this session touched nothing.
        (d / "tasks.toml").write_text("a\nwritten-by-another-session\n")
        r = subprocess.run([sys.executable, str(SCRIPTS / "stop-verify-worktree.py")],
                           cwd=d, input=json.dumps({"cwd": str(d), "session_id": "idle"}),
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(r.returncode, 2, "control: the dirty main tree still blocks")
        self.assertRegex(
            r.stderr.lower(),
            r"attribut|not (yours|your change)|another session|帰属",
            "840c4a17/233f819c: the block never says whose changes these are",
        )


@unittest.skipUnless(OPEN_PINS, OPEN_PIN_REASON + " (90a358d3 == e4a1d386)")
class OpenPin90a358d3ShellSyntaxTestMktemp(unittest.TestCase):
    """90a358d3 == e4a1d386 (STILL OPEN): scripts/tests/check-shell-syntax.sh
    calls `mktemp -d -t <name>` without X's, which GNU mktemp rejects, so 4 of
    its 6 assertions never run on Linux."""

    def test_every_mktemp_template_has_xs(self):
        src = (SCRIPTS / "tests" / "check-shell-syntax.sh").read_text(encoding="utf-8")
        bad = [l.strip() for l in src.splitlines()
               if re.search(r"mktemp\b.*-t\s+\S+", l) and not re.search(r"-t\s+\S*XXX", l)]
        self.assertEqual(bad, [], "90a358d3/e4a1d386: mktemp templates without X's")


@unittest.skipUnless(OPEN_PINS, OPEN_PIN_REASON + " (5cff1d65 == 2240dcdd)")
class OpenPin5cff1d65HooksPathFollowsTheCheckout(unittest.TestCase):
    """5cff1d65 == 2240dcdd (STILL OPEN on this machine): core.hooksPath is the
    ABSOLUTE main-tree path, so a worktree commit runs main's hooks, not its
    own. The documented setting (`git config core.hooksPath .githooks`) is
    relative, which resolves per checkout."""

    def test_hooks_path_resolves_to_this_checkouts_githooks(self):
        r = _git(REPO, "config", "--get", "core.hooksPath", check=False)
        if r.returncode != 0:
            self.fail("core.hooksPath is not set: hooks are not enabled for this checkout")
        configured = r.stdout.strip()
        resolved = (REPO / configured).resolve() if not os.path.isabs(configured) \
            else pathlib.Path(configured).resolve()
        self.assertEqual(resolved, (REPO / ".githooks").resolve(),
                         f"5cff1d65/2240dcdd: core.hooksPath={configured!r} runs another "
                         "checkout's hooks for commits made here")


@unittest.skipUnless(OPEN_PINS, OPEN_PIN_REASON + " (65bfb279, also c3a98510)")
class OpenPin65bfb279WorkspaceTestsAreWired(unittest.TestCase):
    """65bfb279 (+ c3a98510, STILL OPEN): no git hook runs the scripts/ suites
    (check-workspace-tests.py exists but is unwired)."""

    def test_some_git_hook_runs_check_workspace_tests(self):
        hooks = [p for p in (REPO / ".githooks").iterdir() if p.is_file()]
        live = [p.name for p in hooks
                if any("check-workspace-tests.py" in l and not l.lstrip().startswith("#")
                       for l in p.read_text(encoding="utf-8", errors="replace").splitlines())]
        self.assertTrue(live, "65bfb279/c3a98510: no .githooks file runs check-workspace-tests.py")


if __name__ == "__main__":
    unittest.main(verbosity=2)
