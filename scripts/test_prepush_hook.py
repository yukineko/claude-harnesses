#!/usr/bin/env python3
"""Unit tests for .githooks/pre-push's chronically-red-CI check.

CLAUDE.md rule 2-(a) — the implementer of a change must not test their own
implementation. This suite was written by a disinterested test author, from
the CONTRACT below, while someone else edited .githooks/pre-push. The current
(pre-change) content of that file was never read while writing this suite.

Stdlib-only (`unittest`), no third-party dependency, so it runs identically in
CI and locally: `python3 scripts/test_prepush_hook.py`.

The hook is exercised as a SUBPROCESS inside a throwaway git repository that
has its own `scripts/` directory holding a stub `check-ci-red.py` whose exit
code this test chooses, plus a `scripts/gate-bypass.py` stub (always exit 0,
so the hook's earlier ungated-commit check never fires) and deliberately NO
`scripts/check-plugin-rollout.py` (so that advisory block is skipped
entirely). Nothing here touches the real repository, the real checkers, the
real ~/.claude registry, or the git index of any working tree.

CONTRACT pinned here (branching is on the check-ci-red.py EXIT CODE only,
never on its output text):

    checker exit | meaning                       | required hook behaviour
    -------------|-------------------------------|---------------------------
    0            | every workflow judged, none    | push ALLOWED (hook exit 0)
                 | chronic                        |
    1            | chronically red CONFIRMED      | push BLOCKED (non-zero)
    3            | could not be determined        | push ALLOWED (advisory
                 |                                 | carve-out, deliberate)
    other (2,7,  | unrecognized                    | push BLOCKED (non-zero)
    42, ...)     |                                 |

Also pinned, unchanged by the promotion:

  * PREPUSH_SKIP_CI_RED=1 skips the check with NO invocation of the stub at
    all (observed directly: the stub appends to a log file only when run,
    and the log must be absent/empty), and the push is allowed even though
    the stub, if it had run, would have exited 1 (a would-be block).
  * An absent scripts/check-ci-red.py file leaves this check silent (does
    not block on its own account) as long as python3 itself is on PATH.

ANTI-VACUITY CONTROL — the single most important property in this file: a
hook that blocked every push unconditionally would trivially satisfy the
"blocks" rows above while proving nothing. Test0Or3Allows below is that
control: it is the one asserting the exit-0 row and the exit-3 row genuinely
ALLOW the push. Without it, the block-side tests carry no information.

NOT proven by this suite (documented rather than silently assumed):
  * The "python3 absent" sub-case of the silent-degrade row above cannot be
    isolated in a full-hook integration test: the hook's own earlier
    ungated-commit check (scripts/gate-bypass.py) ALSO requires python3 and,
    by its own documented contract, BLOCKS the push when python3 is missing
    (that is correct fail-closed behaviour for that check, not a defect).
    Since `command -v python3` is a single PATH-wide fact for the whole
    script, there is no way to make python3 disappear for the CI-red section
    only while remaining present for the gate-bypass section earlier in the
    same script execution. So this suite proves the "missing SCRIPT file"
    half of that row but not the "missing python3" half in isolation.

The hook under test defaults to `.githooks/pre-push` next to this file's repo
root; set PREPUSH_HOOK_UNDER_TEST to point the same suite at another copy
(used to observe the suite going RED against the previous version of the
hook).
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_REPO = _HERE.parent
_HOOK = Path(
    os.environ.get("PREPUSH_HOOK_UNDER_TEST", _REPO / ".githooks" / "pre-push")
).resolve()

_ZERO_SHA = "0" * 40

_CI_RED_STUB = """#!/usr/bin/env python3
import os, sys
log = os.environ.get("CI_RED_LOG")
if log:
    with open(log, "a") as fh:
        fh.write("check-ci-red.py invoked\\n")
sys.stderr.write("STUB check-ci-red: pretending exit {code}\\n")
sys.exit({code})
"""

_GATE_BYPASS_STUB = """#!/usr/bin/env python3
import sys
sys.exit(0)
"""


def _which(name):
    path = shutil.which(name)
    if path is None:  # pragma: no cover - environment defect
        raise unittest.SkipTest("required tool not on PATH: " + name)
    return path


class HookHarness:
    """A throwaway git repo containing a copy of the hook plus stub scripts."""

    def __init__(self, ci_exit=0, ci_missing=False, with_python=True, skip=False):
        """ci_exit: exit code the check-ci-red.py stub returns.
        ci_missing: if True, never create scripts/check-ci-red.py at all.
        with_python: whether python3 is visible on the hook's PATH.
        skip: if True, set PREPUSH_SKIP_CI_RED=1 in the run environment.
        """
        self.root = Path(tempfile.mkdtemp(prefix="prepush-hook-test-")).resolve()
        self.ci_log = self.root / "ci_red.log"
        # Deliberately do NOT pre-create ci_log — its absence is itself part
        # of what the skip-path test asserts.

        git = _which("git")
        subprocess.run([git, "init", "-q", str(self.root)], check=True, capture_output=True)
        subprocess.run(
            [git, "-C", str(self.root), "config", "user.email", "t@example.invalid"],
            check=True,
            capture_output=True,
        )
        subprocess.run(
            [git, "-C", str(self.root), "config", "user.name", "t"],
            check=True,
            capture_output=True,
        )

        scripts = self.root / "scripts"
        scripts.mkdir()

        gate_bypass = scripts / "gate-bypass.py"
        gate_bypass.write_text(_GATE_BYPASS_STUB)
        gate_bypass.chmod(0o755)

        # Deliberately NOT creating scripts/check-plugin-rollout.py — that
        # advisory block must be skipped entirely without it.

        if not ci_missing:
            ci_red = scripts / "check-ci-red.py"
            ci_red.write_text(_CI_RED_STUB.format(code=ci_exit))
            ci_red.chmod(0o755)

        (self.root / "f.txt").write_text("hi\n")
        subprocess.run([git, "-C", str(self.root), "add", "-A"], check=True, capture_output=True)
        subprocess.run(
            [git, "-C", str(self.root), "commit", "-q", "-m", "init"],
            check=True,
            capture_output=True,
        )
        sha_proc = subprocess.run(
            [git, "-C", str(self.root), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        )
        self.local_sha = sha_proc.stdout.strip()

        self.hook = self.root / "pre-push"
        shutil.copy2(_HOOK, self.hook)
        self.hook.chmod(0o755)

        self.bindir = self.root / "bin"
        self.bindir.mkdir()
        tools = [git, _which("sh"), _which("cat"), _which("grep"), _which("env")]
        if with_python:
            tools.append(_which("python3"))
        for tool in tools:
            os.symlink(tool, self.bindir / Path(tool).name)

        self.env = {
            "PATH": str(self.bindir),
            "HOME": str(self.root),
            "CI_RED_LOG": str(self.ci_log),
            "LC_ALL": "C.UTF-8",
            "GIT_CONFIG_NOSYSTEM": "1",
        }
        if skip:
            self.env["PREPUSH_SKIP_CI_RED"] = "1"

    def run(self):
        stdin = "refs/heads/main %s refs/heads/main %s\n" % (self.local_sha, _ZERO_SHA)
        return subprocess.run(
            [_which("sh"), str(self.hook), "origin", "https://example.invalid/repo.git"],
            cwd=str(self.root),
            env=self.env,
            input=stdin,
            capture_output=True,
            text=True,
        )

    def ci_invoked(self):
        return self.ci_log.exists() and self.ci_log.read_text() != ""

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)


class HookTestCase(unittest.TestCase):
    def harness(self, **kwargs):
        h = HookHarness(**kwargs)
        self.addCleanup(h.cleanup)
        return h

    def assertAllowed(self, proc, msg=""):
        self.assertEqual(
            proc.returncode,
            0,
            "push must be ALLOWED (hook exit 0) %s\n--- stdout ---\n%s\n--- stderr ---\n%s"
            % (msg, proc.stdout, proc.stderr),
        )

    def assertBlocked(self, proc, msg=""):
        self.assertNotEqual(
            proc.returncode,
            0,
            "push must be BLOCKED (non-zero hook exit) %s\n--- stdout ---\n%s\n--- stderr ---\n%s"
            % (msg, proc.stdout, proc.stderr),
        )

    def assertNamesTheCiCheck(self, proc, rc):
        """The stub itself writes 'STUB check-ci-red: pretending exit {rc}' to
        its own stderr; if the hook is doing anything other than a silent
        block, that text (or at minimum the checker's name) must reach the
        developer's terminal. This does not depend on the hook's own wording
        surviving the edit — only on the checker's own diagnostic output
        being surfaced somewhere, which is what "stderr must name the reason"
        means operationally."""
        self.assertIn("check-ci-red", proc.stderr, "stderr must mention the CI check by name")
        self.assertIn(
            str(rc), proc.stderr, "stderr must surface the checker's exit code (%d)" % rc
        )


class Test0Or3Allows(HookTestCase):
    """ANTI-VACUITY CONTROL. A hook that blocks unconditionally would satisfy
    every "blocks" test below for free. This class is what proves it doesn't:
    it asserts the exit-0 row (nothing chronic) and the exit-3 row (could not
    be determined — a deliberate advisory carve-out) both genuinely ALLOW the
    push. Without this class, the rest of the file proves nothing."""

    def test_exit_zero_allows(self):
        h = self.harness(ci_exit=0)
        proc = h.run()
        self.assertAllowed(proc, "when check-ci-red.py exits 0 (nothing chronic)")
        self.assertTrue(h.ci_invoked(), "the checker must actually have run")

    def test_exit_three_allows(self):
        h = self.harness(ci_exit=3)
        proc = h.run()
        self.assertAllowed(proc, "when check-ci-red.py exits 3 (undetermined — advisory)")
        self.assertTrue(h.ci_invoked(), "the checker must actually have run")


class ChronicallyRedBlocks(HookTestCase):
    """Contract row: exit 1 (chronically red CONFIRMED) blocks the push."""

    def test_exit_one_blocks(self):
        h = self.harness(ci_exit=1)
        proc = h.run()
        self.assertBlocked(proc, "when check-ci-red.py exits 1 (chronic)")
        self.assertNamesTheCiCheck(proc, 1)


class UnrecognizedExitBlocks(HookTestCase):
    """Contract row: any exit code other than 0/1/3 is unrecognized and must
    be treated as unsafe-to-ignore, i.e. it blocks."""

    def test_unrecognized_exit_blocks(self):
        for rc in (2, 7, 42):
            with self.subTest(rc=rc):
                h = self.harness(ci_exit=rc)
                proc = h.run()
                self.assertBlocked(proc, "when check-ci-red.py exits unrecognized code %d" % rc)
                self.assertNamesTheCiCheck(proc, rc)


class SkipEnvVarSkipsEntirely(HookTestCase):
    """PREPUSH_SKIP_CI_RED=1 must skip the check with NO invocation at all —
    not merely ignore its result. Proven by the stub's own invocation log
    being absent/empty, and by the push being allowed even though the stub
    (had it run) would have exited 1, a would-be block."""

    def test_skip_prevents_invocation_and_allows_push(self):
        h = self.harness(ci_exit=1, skip=True)
        proc = h.run()
        self.assertAllowed(
            proc, "PREPUSH_SKIP_CI_RED=1 must allow the push even though the stub would exit 1"
        )
        self.assertFalse(
            h.ci_invoked(),
            "the stub must not have been invoked at all under PREPUSH_SKIP_CI_RED=1 "
            "(log file should be absent or empty)",
        )


class MissingScriptDegradesSilently(HookTestCase):
    """An absent scripts/check-ci-red.py leaves this particular check silent
    and does not block on its own account, as long as python3 itself is
    present. (The "python3 itself absent" half of this row cannot be
    isolated in a full-hook run — see the module docstring's NOT-proven
    section: the earlier gate-bypass check shares that same PATH-wide
    dependency and is itself contractually fail-closed on it.)"""

    def test_missing_script_does_not_block(self):
        h = self.harness(ci_missing=True, with_python=True)
        proc = h.run()
        self.assertAllowed(proc, "scripts/check-ci-red.py absent, python3 present")
        self.assertFalse(h.ci_log.exists(), "no stub existed, so nothing could have logged a run")


if __name__ == "__main__":
    unittest.main(verbosity=2)
