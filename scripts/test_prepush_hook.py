#!/usr/bin/env python3
"""Unit tests for .githooks/pre-push's wiring: the one blocking check
(gate-bypass) plus the advisory-only checks (rollout drift, GATE-crate nudge,
autonomy-chain nudge).

Stdlib-only (`unittest`), no third-party dependency, so it runs identically in
CI and locally: `python3 scripts/test_prepush_hook.py`.

The hook is exercised as a SUBPROCESS inside a throwaway git repository that
has its own `scripts/` directory holding a stub `gate-bypass.py` (whose exit
code each test chooses) and, where relevant, a stub `check-plugin-rollout.py`.
Nothing here touches the real repository, the real checkers, the real
~/.claude registry, or the git index of any working tree.

HISTORY: this file used to test a chronically-red-CI check that shelled out to
`scripts/check-ci-red.py` (a checker that queried GitHub Actions). That check
was REMOVED from `.githooks/pre-push`: GitHub Actions is banned in this repo
(CLAUDE.md — never used, no exceptions), so a push-gate predicated on GHA
workflow state had no state left to read. `scripts/check-ci-red.py` itself is
also gone. The tests that exercised it (exit-0/1/3/other, PREPUSH_SKIP_CI_RED,
missing-script degrade) were removed along with it — they asserted behaviour
the hook no longer has. What remains below covers the checks the hook still
performs.

CONTRACT pinned here:

    gate-bypass.py exit | meaning                    | required hook behaviour
    ---------------------|---------------------------|------------------------
    0                     | ledger clean              | push ALLOWED
    1                     | ungated commit outstanding| push BLOCKED (non-zero)
    2                     | could not be determined   | push BLOCKED (non-zero,
                          |                           | fail-closed)
    missing script/python3| cannot check at all       | push BLOCKED (non-zero,
                          |                           | fail-closed)

    check-plugin-rollout.py exit | meaning              | hook behaviour
    ------------------------------|----------------------|------------------
    0                              | nothing to report    | push ALLOWED, silent
    1 (rollout drift)              | committed not rolled | push ALLOWED
                                   | out                  | (advisory prose)
    2 (enablement)                  | plugin not enabled   | push ALLOWED
                                   |                      | (advisory prose)
    3 (unverifiable)                | population incomplete| push ALLOWED
                                   |                      | (advisory prose)
    absent script                   | not installed here   | push ALLOWED, silent

ANTI-VACUITY CONTROL — the single most important property in this file: a hook
that blocked every push unconditionally would trivially satisfy every "blocks"
row above while proving nothing. `test_clean_ledger_allows` is what proves it
doesn't: it asserts the gate-bypass-clean, rollout-absent case genuinely
ALLOWS the push. Without it, the block-side tests carry no information.

The hook under test defaults to `.githooks/pre-push` next to this file's repo
root; set PREPUSH_HOOK_UNDER_TEST to point the same suite at another copy.
"""

from __future__ import annotations

import hashlib
import json
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

# Appends its argv (tab-separated, one line per call) to $GATE_BYPASS_LOG when
# set, so a test can see exactly which --push pairs the hook passed.
_GATE_BYPASS_STUB = """#!/usr/bin/env python3
import os, sys
log = os.environ.get("GATE_BYPASS_LOG")
if log:
    with open(log, "a") as fh:
        fh.write("\\t".join(sys.argv[1:]) + "\\n")
sys.stderr.write("STUB gate-bypass: pretending exit {code}\\n")
sys.exit({code})
"""

_ROLLOUT_STUB = """#!/usr/bin/env python3
import sys
sys.stdout.write("STUB check-plugin-rollout: pretending exit {code}\\n")
sys.exit({code})
"""

# The hook's blocking checks shell out to `cargo check` / `cargo metadata`. The
# throwaway repo is not a real cargo workspace, and running the real compiler
# here would test cargo rather than the hook's wiring, so cargo is stubbed like
# every other external the harness controls. Its exit code for every subcommand
# except `metadata` is the test's to choose, which is what lets both polarities
# be pinned (0 => push allowed, non-zero => blocked).
#
# Every invocation is appended to $CARGO_STUB_LOG (when set) as one line:
#     <cwd> TAB <CARGO_TARGET_DIR or "<unset>"> TAB <arg1> TAB <arg2> ...
# `cargo metadata` answers with the JSON the hook reads package names from
# (packages[].name / manifest_path, target_directory). The package name is
# deliberately NOT the directory name, so a hook that guessed `-p <dir>`
# instead of matching manifest_path is caught.
_CARGO_STUB = """#!/bin/sh
if [ -n "${{CARGO_STUB_LOG:-}}" ]; then
    _l="$(pwd -P)	${{CARGO_TARGET_DIR-<unset>}}"
    for _a in "$@"; do _l="$_l	$_a"; done
    printf '%s\\n' "$_l" >> "$CARGO_STUB_LOG"
fi
if [ "${{1:-}}" = "metadata" ]; then
    cat <<'JSON'
{metadata}
JSON
    exit 0
fi
echo "STUB cargo: pretending exit {code} for: $*" >&2
exit {code}
"""

# A `git` that logs every argv (tab-separated, one line per call) to
# $GIT_WRAPPER_LOG and execs the real git. Used to observe which git
# subcommands the hook runs.
_GIT_WRAPPER = """#!/bin/sh
if [ -n "${{GIT_WRAPPER_LOG:-}}" ]; then
    _l=""
    for _a in "$@"; do _l="$_l	$_a"; done
    printf '%s\\n' "${{_l#	}}" >> "$GIT_WRAPPER_LOG"
fi
exec {real_git} "$@"
"""

# The compile-fail stub committed by the init commit: exits 0, and appends one
# line to $CF_LOG when that is set, so a test can see whether it ran.
_COMPILE_FAIL_STUB = """#!/usr/bin/env python3
import os, sys
log = os.environ.get("CF_LOG")
if log:
    with open(log, "a") as fh:
        fh.write("ran in " + os.getcwd() + "\\n")
sys.exit(0)
"""

# The crate a `crate=True` harness commits, and the package name its Cargo.toml
# (and the cargo stub's metadata) gives it.
CRATE_DIR = "crates/foo"
CRATE_PKG = "foo-pkg"

# The hook runs the repo's own scripts/test-pass-record.py, so the harness
# copies (and commits) it into the throwaway repo when it exists.
_PASS_RECORD = Path(
    os.environ.get("TEST_PASS_RECORD_UNDER_TEST", _REPO / "scripts" / "test-pass-record.py")
).resolve()


def _which(name):
    path = shutil.which(name)
    if path is None:  # pragma: no cover - environment defect
        raise unittest.SkipTest("required tool not on PATH: " + name)
    return path


class HookHarness:
    """A throwaway git repo containing a copy of the hook plus stub scripts."""

    def __init__(
        self,
        gate_bypass_exit=0,
        gate_bypass_missing=False,
        with_python=True,
        rollout_exit=None,
        rollout_missing=True,
        cargo_exit=0,
        cargo_missing=False,
        crate=False,
        git_log=False,
    ):
        """gate_bypass_exit: exit code the gate-bypass.py stub returns.
        gate_bypass_missing: if True, never create scripts/gate-bypass.py at all.
        with_python: whether python3 is visible on the hook's PATH.
        rollout_exit: exit code the check-plugin-rollout.py stub returns; only
            used when rollout_missing is False.
        rollout_missing: if True (default), never create
            scripts/check-plugin-rollout.py — that advisory block must then be
            skipped entirely.
        cargo_exit: exit code the `cargo` stub returns. The hook type-checks the
            pushed commits (f98de400, 2026-08-06) and BLOCKS when that fails, so
            every test about some later stage has to get past it. Stubbed rather
            than run for real because this harness deliberately isolates HOME
            and PATH: the rustup shim then has no ~/.rustup to read a default
            toolchain from, and the throwaway repo has no Cargo.toml to check.
        cargo_missing: if True, keep `cargo` off the hook's PATH entirely, so
            the fail-closed "no toolchain, so block" branch can still be
            exercised — the reason this is a knob and not an unconditional stub.
        crate: if True, the init commit also adds crates/foo/Cargo.toml (package
            CRATE_PKG) and crates/foo/src/lib.rs. Since the 2026-11-08 ruling
            the hook runs `cargo check` only when the pushed range touches a
            crate or a root cargo file, so a test that needs cargo to run (or
            to block) must set this.
        git_log: if True, `git` on the hook's PATH is a wrapper that logs every
            argv to self.git_log before exec'ing the real git.
        """
        self.root = Path(tempfile.mkdtemp(prefix="prepush-hook-test-")).resolve()
        # Logs live OUTSIDE the repo so they never make the tree dirty.
        self.logdir = Path(tempfile.mkdtemp(prefix="prepush-hook-logs-")).resolve()
        self.cargo_log = self.logdir / "cargo.log"
        self.git_log = self.logdir / "git.log"
        self.gate_bypass_log = self.logdir / "gate-bypass.log"
        self.cf_log = self.logdir / "compile-fail.log"

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

        if not gate_bypass_missing:
            gate_bypass = scripts / "gate-bypass.py"
            gate_bypass.write_text(_GATE_BYPASS_STUB.format(code=gate_bypass_exit))
            gate_bypass.chmod(0o755)

        if not rollout_missing:
            rollout = scripts / "check-plugin-rollout.py"
            rollout.write_text(_ROLLOUT_STUB.format(code=rollout_exit))
            rollout.chmod(0o755)

        # The hook BLOCKS any pushed commit whose TREE lacks
        # scripts/check-compile-fail.py (backlog 034b6620, d257e44d), running
        # the committed copy from a detached checkout of that commit. Like
        # cargo, the trybuild contracts are not this suite's subject, so the
        # init commit carries a stub that passes; the hook's own fail-closed
        # branch for an absent script is untouched.
        compile_fail = scripts / "check-compile-fail.py"
        compile_fail.write_text(_COMPILE_FAIL_STUB)
        compile_fail.chmod(0o755)

        if _PASS_RECORD.is_file():
            shutil.copy2(_PASS_RECORD, scripts / "test-pass-record.py")

        if crate:
            src = self.root / CRATE_DIR / "src"
            src.mkdir(parents=True)
            (self.root / CRATE_DIR / "Cargo.toml").write_text(
                '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n' % CRATE_PKG
            )
            (src / "lib.rs").write_text("pub fn f() {}\n")

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
        tools = [_which("sh"), _which("cat"), _which("grep"), _which("env")]
        # Ordinary POSIX utilities a shell hook may reasonably use. Present so a
        # block is about the hook's logic, not about a utility this harness
        # happened to leave off PATH (python3 and cargo stay knobs above).
        for extra in ("sed", "sort", "tr", "head", "tail", "cut", "uniq", "awk",
                      "wc", "mktemp", "dirname", "basename", "mkdir", "rm", "date",
                      "ls", "printf", "test", "mv", "cp", "find"):
            found = shutil.which(extra)
            if found:
                tools.append(found)
        if with_python:
            tools.append(_which("python3"))
        for tool in tools:
            link = self.bindir / Path(tool).name
            if not link.exists():
                os.symlink(tool, link)
        if git_log:
            wrapper = self.bindir / "git"
            wrapper.write_text(_GIT_WRAPPER.format(real_git=git))
            wrapper.chmod(0o755)
        else:
            os.symlink(git, self.bindir / "git")

        if not cargo_missing:
            pkgs = []
            if crate:
                pkgs.append({
                    "name": CRATE_PKG,
                    "version": "0.1.0",
                    "id": CRATE_PKG + " 0.1.0",
                    "manifest_path": str(self.root / CRATE_DIR / "Cargo.toml"),
                })
            metadata = json.dumps({
                "packages": pkgs,
                "workspace_members": [p["id"] for p in pkgs],
                "target_directory": str(self.root / "target"),
                "workspace_root": str(self.root),
                "version": 1,
            })
            cargo = self.bindir / "cargo"
            cargo.write_text(_CARGO_STUB.format(code=cargo_exit, metadata=metadata))
            cargo.chmod(0o755)

        self.env = {
            "PATH": str(self.bindir),
            "HOME": str(self.root),
            "LC_ALL": "C.UTF-8",
            "GIT_CONFIG_NOSYSTEM": "1",
            "CARGO_STUB_LOG": str(self.cargo_log),
            "GIT_WRAPPER_LOG": str(self.git_log),
            "GATE_BYPASS_LOG": str(self.gate_bypass_log),
            "CF_LOG": str(self.cf_log),
        }

    # --- helpers for the range-scope tests ---------------------------------
    def git(self, *args):
        return subprocess.run(
            [_which("git"), "-C", str(self.root), *args],
            check=True, capture_output=True, text=True,
        ).stdout.strip()

    def commit_files(self, files, message="change"):
        """Write {relpath: text} (None deletes), commit, return the new HEAD."""
        for rel, text in files.items():
            p = self.root / rel
            if text is None:
                p.unlink()
            else:
                p.parent.mkdir(parents=True, exist_ok=True)
                p.write_text(text)
        self.git("add", "-A", "--", *files.keys())
        self.git("commit", "-q", "-m", message)
        self.local_sha = self.git("rev-parse", "HEAD")
        return self.local_sha

    def tree_of(self, sha):
        return self.git("rev-parse", sha + "^{tree}")

    def cargo_calls(self):
        """[(cwd, CARGO_TARGET_DIR, [args...]), ...] in call order."""
        if not self.cargo_log.is_file():
            return []
        out = []
        for line in self.cargo_log.read_text().splitlines():
            parts = line.split("\t")
            out.append((parts[0], parts[1], parts[2:]))
        return out

    def cargo_checks(self):
        return [c for c in self.cargo_calls() if c[2][:1] == ["check"]]

    def git_calls(self):
        if not self.git_log.is_file():
            return []
        return [ln.split("\t") for ln in self.git_log.read_text().splitlines()]

    def gate_bypass_calls(self):
        if not self.gate_bypass_log.is_file():
            return []
        return [ln.split("\t") if ln else [] for ln in self.gate_bypass_log.read_text().split("\n")[:-1]]

    def record_path(self, tree, test_id):
        """Where the spec puts a pass record in the default store."""
        return (self.root / ".git" / "test-pass-record"
                / hashlib.sha256(test_id.encode()).hexdigest() / (tree + ".json"))

    def write_record(self, tree, test_id, content=None):
        p = self.record_path(tree, test_id)
        p.parent.mkdir(parents=True, exist_ok=True)
        if content is None:
            content = json.dumps({"version": 1, "tree": tree, "test": test_id})
        p.write_text(content)
        return p

    def run(self, cwd=None, stdin=None):
        if stdin is None:
            stdin = "refs/heads/main %s refs/heads/main %s\n" % (self.local_sha, _ZERO_SHA)
        return subprocess.run(
            [_which("sh"), str(self.hook), "origin", "https://example.invalid/repo.git"],
            cwd=str(cwd if cwd is not None else self.root),
            env=self.env,
            input=stdin,
            capture_output=True,
            text=True,
        )

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)
        shutil.rmtree(self.logdir, ignore_errors=True)


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


class CleanLedgerAllows(HookTestCase):
    """ANTI-VACUITY CONTROL. A hook that blocked every push unconditionally
    would satisfy every "blocks" test below for free. This is what proves it
    doesn't: a clean gate-bypass ledger (exit 0) and no rollout script present
    genuinely ALLOWS the push."""

    def test_clean_ledger_allows(self):
        h = self.harness(gate_bypass_exit=0)
        proc = h.run()
        self.assertAllowed(proc, "when gate-bypass.py exits 0 (ledger clean)")


class OutstandingBypassBlocks(HookTestCase):
    """Contract row: an outstanding ungated commit (gate-bypass exit 1)
    blocks the push."""

    def test_outstanding_bypass_blocks(self):
        h = self.harness(gate_bypass_exit=1)
        proc = h.run()
        self.assertBlocked(proc, "when gate-bypass.py exits 1 (outstanding bypass)")
        self.assertIn(
            "blocked",
            proc.stderr.lower(),
            "stderr must explain the push is blocked",
        )


class UndeterminedBypassBlocks(HookTestCase):
    """Contract row: gate-bypass.py unable to determine the verdict (exit 2)
    is fail-closed — it blocks rather than assuming clean."""

    def test_undetermined_bypass_blocks(self):
        h = self.harness(gate_bypass_exit=2)
        proc = h.run()
        self.assertBlocked(proc, "when gate-bypass.py exits 2 (undetermined)")


class MissingBypassScriptBlocks(HookTestCase):
    """Fail-closed: an absent scripts/gate-bypass.py means the hook cannot
    check for ungated commits at all, so it blocks rather than assuming
    clean."""

    def test_missing_script_blocks(self):
        h = self.harness(gate_bypass_missing=True)
        proc = h.run()
        self.assertBlocked(proc, "scripts/gate-bypass.py absent")


class RolloutAdvisoryNeverBlocks(HookTestCase):
    """Contract row: whatever check-plugin-rollout.py reports (drift,
    enablement, unverifiable, or an unrecognized code), it is advisory only —
    the push is never blocked on its account."""

    def test_rollout_drift_is_advisory(self):
        h = self.harness(gate_bypass_exit=0, rollout_missing=False, rollout_exit=1)
        proc = h.run()
        self.assertAllowed(proc, "rollout drift (exit 1) must be advisory only")
        self.assertIn("rollout", proc.stderr.lower())

    def test_rollout_enablement_is_advisory(self):
        h = self.harness(gate_bypass_exit=0, rollout_missing=False, rollout_exit=2)
        proc = h.run()
        self.assertAllowed(proc, "rollout enablement class (exit 2) must be advisory only")
        self.assertIn("enablement", proc.stderr.lower())

    def test_rollout_unverifiable_is_advisory(self):
        h = self.harness(gate_bypass_exit=0, rollout_missing=False, rollout_exit=3)
        proc = h.run()
        self.assertAllowed(proc, "rollout unverifiable class (exit 3) must be advisory only")
        self.assertIn("unverifiable", proc.stderr.lower())

    def test_missing_rollout_script_is_silent(self):
        h = self.harness(gate_bypass_exit=0, rollout_missing=True)
        proc = h.run()
        self.assertAllowed(proc, "scripts/check-plugin-rollout.py absent")
        self.assertNotIn("rollout", proc.stderr.lower())


class NotAGitWorkTreeBlocks(HookTestCase):
    """CLAUDE.md 3 (判定不能は制限側へ). The hook's very first act is

        REPO="$(git rev-parse --show-toplevel 2>/dev/null)" || exit 0

    Until this test landed, that `|| exit 0` waved the push through whenever the
    repo root could not be resolved — skipping the ungated-commit ledger check
    AND the compiler check on precisely the input the hook could not understand.
    Same for the `cd "$REPO" || exit 0` on the next line. "Could not determine"
    is not "clean", and the twin gate `.githooks/pre-commit` already resolves the
    identical probe to `exit 1`.
    """

    def test_outside_any_work_tree_blocks(self):
        h = self.harness(gate_bypass_exit=0)
        outside = Path(tempfile.mkdtemp(prefix="prepush-not-a-repo-")).resolve()
        self.addCleanup(shutil.rmtree, str(outside), True)
        proc = h.run(cwd=outside)
        self.assertBlocked(proc, "cwd is not inside any git work tree")
        self.assertIn("work tree", proc.stderr.lower())


class CargoTypecheckGate(HookTestCase):
    """The hook's second blocking check: the pushed tip must type-check.

    Both polarities are pinned here because the four "advisory" tests above are
    ALLOW assertions — they were silently red from the day the cargo block
    landed until the harness gained a cargo stub, which meant the whole suite's
    anti-vacuity half was dead and only the "blocks" rows were being exercised.

    Since the 2026-11-08 ruling (hooks check only the change's range) cargo
    runs only when the pushed range touches a crate, so these commits touch
    crates/foo (crate=True). Without that the range holds no cargo input and
    cargo would never be asked.
    """

    def test_cargo_failure_blocks(self):
        h = self.harness(gate_bypass_exit=0, cargo_exit=1, crate=True)
        proc = h.run()
        self.assertBlocked(proc, "cargo check exited non-zero")
        self.assertTrue(h.cargo_checks(), "the block must come from cargo check\n" + proc.stderr)

    def test_cargo_success_allows_control(self):
        """Control for the row above: same fixture, cargo exits 0."""
        h = self.harness(gate_bypass_exit=0, cargo_exit=0, crate=True)
        proc = h.run()
        self.assertAllowed(proc, "cargo check exited 0")
        self.assertTrue(h.cargo_checks(), "cargo check must have run\n" + proc.stderr)

    def test_cargo_missing_blocks(self):
        h = self.harness(gate_bypass_exit=0, cargo_missing=True, crate=True)
        proc = h.run()
        self.assertBlocked(proc, "no cargo on PATH — cannot type-check")
        self.assertIn("cargo not found", proc.stderr.lower())


if __name__ == "__main__":
    unittest.main(verbosity=2)
