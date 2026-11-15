#!/usr/bin/env python3
"""RED acceptance tests for GitHub #1552: scripts/gate-severity-probe.py --json.

Pins the CLI contract documented in docs/specs/gate-severity-probe.md.  The
script does not exist yet, so every probe test fails today; each one asserts a
specific contract property (not mere existence) so it cannot go green until
that property holds.  Strict xfail => the marker must be removed when the
feature lands.

Run:  python3 -m pytest scripts/test_gate_severity_probe.py

Speed: only the cheap `launcher-exec-bit` gate is probed (via `--gate`).
The "caller" repo is a throwaway local clone of this checkout's HEAD, so the
tests never touch the real repo; --repo points the probe at it.

Binding user rulings (2026-11-08): the probe judges only the change's own
range, and looks at no worktree except the throwaway ones it creates and
removes itself.
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

import pytest

REPO = Path(__file__).resolve().parent.parent
PROBE = REPO / "scripts" / "gate-severity-probe.py"
GATE = "launcher-exec-bit"
LEAKS = {"local-commit", "push", "rollout", "undetermined"}
RED = pytest.mark.xfail(strict=True, reason="RED: GitHub #1552")


def git(cwd, *args, check=True):
    return subprocess.run(
        ["git", "-C", str(cwd), *args], capture_output=True, text=True, check=check
    )


def snapshot(repo):
    """Everything the probe must leave untouched in the caller repo."""
    return {
        "head": git(repo, "rev-parse", "HEAD").stdout,
        "branch": git(repo, "symbolic-ref", "-q", "HEAD", check=False).stdout,
        "index": git(repo, "ls-files", "-s").stdout,
        "cached_diff": git(repo, "diff", "--cached").stdout,
        "status": git(repo, "status", "--porcelain=v1", "-uall").stdout,
        "worktrees": git(repo, "worktree", "list", "--porcelain").stdout,
        "wt_admin": sorted(
            p.name for p in (Path(git(repo, "rev-parse", "--git-common-dir").stdout.strip())
                             if Path(git(repo, "rev-parse", "--git-common-dir").stdout.strip()).is_absolute()
                             else repo / git(repo, "rev-parse", "--git-common-dir").stdout.strip()
                             ).joinpath("worktrees").glob("*")
        ),
    }


def clone_repo(prefix):
    root = Path(tempfile.mkdtemp(prefix=prefix)).resolve()
    dest = root / "caller"
    subprocess.run(
        ["git", "clone", "-q", "--local", str(REPO), str(dest)],
        check=True, capture_output=True,
    )
    git(dest, "config", "user.name", "probe-test")
    git(dest, "config", "user.email", "probe-test@example.invalid")
    git(dest, "config", "core.fileMode", "false")
    return root, dest


def run_probe(repo, *extra):
    return subprocess.run(
        [sys.executable, str(PROBE), "--json", "--repo", str(repo), *extra],
        capture_output=True, text=True, cwd=str(repo), timeout=600,
    )


def probe_json(case, repo, *extra):
    proc = run_probe(repo, *extra)
    case.assertEqual(
        proc.returncode, 0,
        "probe must exit 0 and emit JSON\n--- stdout ---\n%s\n--- stderr ---\n%s"
        % (proc.stdout, proc.stderr),
    )
    return json.loads(proc.stdout)


def entry_for(case, doc, gate=GATE):
    case.assertIn("gates", doc)
    found = [g for g in doc["gates"] if g.get("gate") == gate]
    case.assertEqual(len(found), 1, "exactly one entry for %s in %r" % (gate, doc))
    return found[0]


class ProbeCase(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root, cls.caller = clone_repo("gate-severity-probe-")

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, ignore_errors=True)


class Controls(ProbeCase):
    """Pass today: prove the fixture is meaningful and the caller is sound."""

    def test_caller_clone_is_clean_and_has_the_gate(self):
        self.assertEqual(git(self.caller, "status", "--porcelain").stdout, "")
        self.assertTrue((self.caller / "scripts" / "check-launcher-exec-bit.py").is_file())

    def test_the_gate_really_fires_on_a_non_executable_launcher(self):
        """Independent of the probe: the representative violation (a launcher
        staged 100644) must make the gate itself exit non-zero, and a clean
        index must make it exit 0.  Otherwise 'fires' tests prove nothing."""
        script = self.caller / "scripts" / "check-launcher-exec-bit.py"
        clean = subprocess.run([sys.executable, str(script), "--staged"],
                               cwd=self.caller, capture_output=True, text=True)
        self.assertEqual(clean.returncode, 0, clean.stderr)
        launcher = next(iter(sorted(self.caller.glob("crates/*/bin/*"))))
        rel = launcher.relative_to(self.caller).as_posix()
        git(self.caller, "update-index", "--chmod=-x", rel)
        try:
            bad = subprocess.run([sys.executable, str(script), "--staged"],
                                 cwd=self.caller, capture_output=True, text=True)
            self.assertEqual(bad.returncode, 1, bad.stdout + bad.stderr)
        finally:
            git(self.caller, "update-index", "--chmod=+x", rel)
        self.assertEqual(git(self.caller, "diff", "--cached").stdout, "")


class ContractShape(ProbeCase):
    def test_entry_has_fires_caught_by_leak(self):
        e = entry_for(self, probe_json(self, self.caller, "--gate", GATE))
        self.assertIn(e["fires"], (True, False, "undetermined"))
        self.assertIsInstance(e["caught_by"], list)
        self.assertTrue(all(isinstance(x, str) for x in e["caught_by"]))
        self.assertTrue(e["leak"] is None or e["leak"] in LEAKS, e["leak"])

    def test_representative_violation_fires_the_gate(self):
        e = entry_for(self, probe_json(self, self.caller, "--gate", GATE))
        self.assertIs(e["fires"], True)

    def test_caught_by_never_lists_the_disabled_gate_and_leak_follows_it(self):
        e = entry_for(self, probe_json(self, self.caller, "--gate", GATE))
        self.assertNotIn(GATE, e["caught_by"], "gate was disabled in that run")
        if e["caught_by"]:
            self.assertIsNone(e["leak"], "caught by another layer => no leak")
        else:
            self.assertIn(e["leak"], LEAKS, "nothing catches it => a named leak level")

    def test_gate_filter_returns_only_that_gate_and_unknown_gate_is_rejected(self):
        doc = probe_json(self, self.caller, "--gate", GATE)
        self.assertEqual([g["gate"] for g in doc["gates"]], [GATE])
        bad = run_probe(self.caller, "--gate", "no-such-gate")
        self.assertNotEqual(bad.returncode, 0, "unknown gate must not look like an empty/clean result")
        self.assertNotIn('"gates": []', bad.stdout)


class UndeterminedIsNeverFolded(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.root, cls.caller = clone_repo("gate-severity-probe-broken-")
        # Make the gate's own scanner absent: the baseline (no fixture) run
        # already blocks on this gate, so the probe cannot attribute a block
        # to the injected violation.  That is UNDETERMINED, not fires=true.
        git(cls.caller, "rm", "-q", "scripts/check-launcher-exec-bit.py")
        git(cls.caller, "commit", "-q", "-m", "remove scanner", "--no-verify")

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.root, ignore_errors=True)

    def test_unattributable_block_is_undetermined_not_false_and_not_null(self):
        e = entry_for(self, probe_json(self, self.caller, "--gate", GATE))
        self.assertEqual(e["fires"], "undetermined")
        self.assertIsNot(e["fires"], False)
        self.assertEqual(e["leak"], "undetermined")
        self.assertIsNotNone(e["leak"])


class NeverTouchesCaller(ProbeCase):
    def test_head_index_status_and_worktrees_unchanged_with_dirty_caller(self):
        # Give the caller real in-flight state: a staged change plus an
        # untracked file.  The probe must preserve both byte for byte.
        (self.caller / "staged-note.txt").write_text("staged\n")
        git(self.caller, "add", "staged-note.txt")
        (self.caller / "untracked-note.txt").write_text("untracked\n")
        try:
            before = snapshot(self.caller)
            self.assertIn("staged-note.txt", before["cached_diff"])  # fixture is live
            doc = probe_json(self, self.caller, "--gate", GATE)
            entry_for(self, doc)  # the probe really ran (not a vacuous no-op)
            after = snapshot(self.caller)
            self.assertEqual(after, before)
            self.assertTrue((self.caller / "untracked-note.txt").is_file())
        finally:
            git(self.caller, "reset", "-q", "HEAD", "--", "staged-note.txt", check=False)
            for n in ("staged-note.txt", "untracked-note.txt"):
                (self.caller / n).unlink(missing_ok=True)

    def test_throwaway_worktrees_are_removed_and_no_sibling_is_consulted(self):
        sibling = self.root / "sibling"
        git(self.caller, "worktree", "add", "-q", "-b", "sibling-wt", str(sibling))
        # Poison the sibling with a launcher-exec-bit violation: if the probe
        # looked at it, the baseline would block and fires would degrade.
        launcher = next(iter(sorted(sibling.glob("crates/*/bin/*"))))
        git(sibling, "update-index", "--chmod=-x", launcher.relative_to(sibling).as_posix())
        sib_before = snapshot(sibling)
        try:
            before = git(self.caller, "worktree", "list", "--porcelain").stdout
            e = entry_for(self, probe_json(self, self.caller, "--gate", GATE))
            self.assertIs(e["fires"], True, "sibling worktree state must not leak into the verdict")
            self.assertEqual(git(self.caller, "worktree", "list", "--porcelain").stdout, before,
                             "throwaway worktrees must be removed")
            self.assertEqual(snapshot(sibling), sib_before)
        finally:
            git(self.caller, "worktree", "remove", "--force", str(sibling), check=False)
            git(self.caller, "branch", "-D", "sibling-wt", check=False)


if __name__ == "__main__":
    raise SystemExit(pytest.main([__file__]))
