#!/usr/bin/env python3
"""Unit tests for the `--no-verify` bypass-closing arrangement.

Stdlib-only (`unittest`), same shape as `scripts/test_precommit_hook.py`: every
assertion is made against a THROWAWAY `git init` repository holding copies of
the hooks and stub scanners whose exit codes this test chooses.  Nothing here
runs a stub against the real repository, and nothing here writes to it.

    python3 scripts/test_gate_bypass.py

What is under test (one property each)

  .githooks/pre-commit        on green, certifies the tree it inspected into a
                              PER-WORKTREE sentinel, and clears the bypass
                              ledger
  .githooks/post-commit       compares the committed tree against that sentinel;
                              mismatch or absence appends to a REPO-WIDE ledger
  .githooks/pre-merge-commit  execs pre-commit, because a true merge commit runs
                              pre-merge-commit and NOT pre-commit
  .githooks/post-merge        execs post-commit for a real merge commit, because
                              post-commit does NOT run on a merge at all
  .githooks/pre-push          blocks while the ledger is non-empty, and blocks
                              when it cannot run the check at all
  scripts/gate-bypass.py      ledger consumer: 0 clean / 1 outstanding /
                              2 undetermined
  scripts/deny-no-verify.py   PreToolUse hook refusing the bypass at the Bash
                              call, before git ever sees it

Point the suite at a MUTATED copy to observe it going RED:

    GATE_HOOKS_UNDER_TEST=/tmp/mutant/.githooks \\
    GATE_SCRIPTS_UNDER_TEST=/tmp/mutant/scripts \\
    python3 scripts/test_gate_bypass.py

`KnownDefects` at the bottom is NOT a pass list.  Each test there pins behaviour
that was OBSERVED and is WRONG, so that the defect is recorded in executable
form instead of only in a report.  Every one of them must be inverted when the
corresponding defect is fixed; the docstrings say so individually.
"""

from __future__ import annotations

import json
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_REPO = _HERE.parent

_HOOKS_DIR = Path(
    os.environ.get("GATE_HOOKS_UNDER_TEST", _REPO / ".githooks")
).resolve()
_SCRIPTS_DIR = Path(
    os.environ.get("GATE_SCRIPTS_UNDER_TEST", _REPO / "scripts")
).resolve()

HOOKS = (
    "pre-commit",
    "post-commit",
    "pre-merge-commit",
    "post-merge",
    "pre-push",
)

SCANNERS = [
    "check-prompt-injection.py",
    "check-fail-open.py",
    "check-doc-claims.py",
    "check-test-weakening.py",
    "check-plugin-versions.py",
    "check-version-bumped.py",
]

# A stub scanner: records that it ran, then exits with the code baked in here.
# It never reads the repository, so it can only ever report what this test told
# it to report.
_STUB = """#!/usr/bin/env python3
import os, sys
log = os.environ.get("GATE_TEST_LOG")
if log:
    with open(log, "a") as fh:
        fh.write({name!r} + "\\n")
sys.exit({code})
"""


def _which(name):
    path = shutil.which(name)
    if path is None:  # pragma: no cover - environment defect
        raise unittest.SkipTest("required tool not on PATH: " + name)
    return path


class GateHarness:
    """A throwaway repo wired up exactly like a clone that ran
    `git config core.hooksPath .githooks`."""

    def __init__(self, exits=None, with_gate_bypass=True):
        # .resolve() so paths match what git reports on macOS (/var ->
        # /private/var); otherwise the sentinel comparison would be testing
        # string normalisation rather than the hook.
        self.root = Path(tempfile.mkdtemp(prefix="gate-bypass-test-")).resolve()
        # OUTSIDE the repo: `git add -A` would otherwise commit it, and a
        # dirty ran.log blocks `git checkout`.
        self.log = self.root.parent / (self.root.name + "-ran.log")
        self.log.write_text("")
        self.env_extra_cleanup = self.log

        self.env = dict(
            PATH=os.environ.get("PATH", "/usr/bin:/bin"),
            HOME=str(self.root),
            TMPDIR=str(self.root),
            GATE_TEST_LOG=str(self.log),
            LC_ALL="C",
            GIT_CONFIG_NOSYSTEM="1",
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_TERMINAL_PROMPT="0",
            GIT_CEILING_DIRECTORIES=str(self.root.parent),
            GIT_AUTHOR_NAME="t",
            GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t",
            GIT_COMMITTER_EMAIL="t@example.invalid",
        )

        self._run([_which("git"), "init", "-q", "-b", "main", str(self.root)], cwd=None)

        (self.root / ".githooks").mkdir()
        for hook in HOOKS:
            dst = self.root / ".githooks" / hook
            shutil.copy2(_HOOKS_DIR / hook, dst)
            dst.chmod(0o755)

        scripts = self.root / "scripts"
        scripts.mkdir()
        if with_gate_bypass:
            shutil.copy2(_SCRIPTS_DIR / "gate-bypass.py", scripts / "gate-bypass.py")
        for scanner in SCANNERS:
            self.set_stub(scanner, 0)

        self.git("config", "core.hooksPath", ".githooks")
        self.git("config", "commit.gpgsign", "false")
        self.git("config", "advice.detachedHead", "false")

        # Seed commit.  Made through the gate on purpose: it is also the control
        # for "a plain commit leaves nothing behind".
        (self.root / "seed.txt").write_text("seed\n")
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "seed")
        self.clear_log()

        # Only now: the seed commit has to get through the gate.
        for scanner, code in (exits or {}).items():
            self.set_stub(scanner, code)

    # --- construction helpers ------------------------------------------
    def set_stub(self, scanner, code):
        target = self.root / "scripts" / scanner
        target.write_text(_STUB.format(name=scanner, code=code))
        target.chmod(0o755)

    def _run(self, argv, cwd="<root>", stdin=None, check=False):
        return subprocess.run(
            argv,
            cwd=str(self.root) if cwd == "<root>" else (str(cwd) if cwd else None),
            env=self.env,
            input=stdin,
            capture_output=True,
            text=True,
            check=check,
        )

    # --- git ------------------------------------------------------------
    def git(self, *args, cwd="<root>", check=True):
        proc = self._run([_which("git")] + list(args), cwd=cwd)
        if check and proc.returncode != 0:
            raise AssertionError(
                "git %s failed (%d)\n--- stdout ---\n%s\n--- stderr ---\n%s"
                % (" ".join(args), proc.returncode, proc.stdout, proc.stderr)
            )
        return proc

    def write(self, relpath, text, cwd=None):
        target = (Path(cwd) if cwd else self.root) / relpath
        target.parent.mkdir(parents=True, exist_ok=True)
        target.write_text(text)

    def commit(self, message, *extra, cwd="<root>"):
        return self.git("commit", "-q", "-m", message, *extra, cwd=cwd, check=False)

    # --- state under test -------------------------------------------------
    @property
    def sentinel(self):
        return self.root / ".git" / "gate-verified-tree"

    @property
    def ledger(self):
        return self.root / ".git" / "gate-bypassed"

    def ledger_lines(self):
        if not self.ledger.exists():
            return []
        return [ln for ln in self.ledger.read_text().splitlines() if ln.strip()]

    def ran(self):
        return [ln for ln in self.log.read_text().splitlines() if ln]

    def clear_log(self):
        self.log.write_text("")

    # --- the consumers ----------------------------------------------------
    def gate_bypass(self, *args, cwd="<root>"):
        return self._run(
            [_which("python3"), str(self.root / "scripts" / "gate-bypass.py")]
            + list(args),
            cwd=cwd,
        )

    def pre_push(self, stdin="", cwd="<root>"):
        return self._run(
            [str(self.root / ".githooks" / "pre-push"), "origin", "/dev/null"],
            cwd=cwd,
            stdin=stdin,
        )

    def pre_commit(self, cwd="<root>"):
        return self._run([str(self.root / ".githooks" / "pre-commit")], cwd=cwd)

    def cleanup(self):
        shutil.rmtree(self.root, ignore_errors=True)
        try:
            self.log.unlink()
        except OSError:
            pass


class GateTestCase(unittest.TestCase):
    def harness(self, **kwargs):
        h = GateHarness(**kwargs)
        self.addCleanup(h.cleanup)
        return h

    def assertLedgerEmpty(self, h, why=""):
        self.assertEqual(
            h.ledger_lines(), [], "bypass ledger must be empty %s" % why
        )

    def assertLedgerHas(self, h, n, why=""):
        lines = h.ledger_lines()
        self.assertEqual(
            len(lines),
            n,
            "expected %d ledger entr(y/ies) %s, got:\n%s" % (n, why, "\n".join(lines)),
        )


# ---------------------------------------------------------------------------
# 1. The control case.  Without this, everything below is satisfied by a hook
#    that records every commit as a bypass.
# ---------------------------------------------------------------------------
class PlainCommitIsClean(GateTestCase):
    def test_plain_commit_leaves_ledger_empty(self):
        h = self.harness()
        h.write("a.txt", "one\n")
        h.git("add", "-A")
        proc = h.commit("plain")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(h.ran(), SCANNERS, "every gate must have run")
        self.assertLedgerEmpty(h, "after a gated commit")
        self.assertFalse(
            h.ledger.exists(), "a gated commit must not even create the ledger"
        )

    def test_plain_commit_consumes_the_sentinel(self):
        """A surviving sentinel is what lets a LATER ungated commit of the same
        tree certify itself, so consuming it is load-bearing, not tidiness."""
        h = self.harness()
        h.write("a.txt", "one\n")
        h.git("add", "-A")
        h.commit("plain")
        self.assertFalse(
            h.sentinel.exists(),
            "post-commit must consume the sentinel it matched; it is still at %s"
            % h.sentinel,
        )

    def test_precommit_writes_the_tree_and_the_head_it_judged_against(self):
        """Observed directly: run the hook alone and compare what it wrote
        against the tree git would commit and the HEAD it was judged at."""
        h = self.harness()
        h.write("a.txt", "one\n")
        h.git("add", "-A")
        proc = h.pre_commit()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(h.sentinel.exists(), "green pre-commit must certify a tree")
        self.assertEqual(
            h.sentinel.read_text().split(),
            [
                h.git("write-tree").stdout.strip(),
                h.git("rev-parse", "HEAD").stdout.strip(),
            ],
            "the certificate must name the tree AND the HEAD it was judged "
            "against — two of the six gates read a diff, not a snapshot",
        )

    def test_certificate_does_not_survive_head_moving(self):
        """The certificate is a claim about a DIFF (tree vs HEAD), so it must
        stop being valid when HEAD moves underneath it.  Simulates the `pull`
        case from the hook's own comment: same index tree, different parent."""
        h = self.harness()
        h.write("a.txt", "one\n")
        h.git("add", "-A")
        self.assertEqual(h.pre_commit().returncode, 0)
        old_head = h.git("rev-parse", "HEAD").stdout.strip()

        # Move HEAD forward without touching the index: a commit with the same
        # tree, made through plumbing so no hook runs and the sentinel survives.
        other_tree = h.git("rev-parse", "HEAD^{tree}").stdout.strip()
        new_head = h.git(
            "commit-tree", other_tree, "-p", old_head, "-m", "someone else's commit"
        ).stdout.strip()
        h.git("update-ref", "HEAD", new_head)
        self.assertTrue(h.sentinel.exists(), "precondition: certificate survived")

        h.commit("ungated at a HEAD the gate never judged", "--no-verify")
        self.assertLedgerHas(
            h,
            1,
            "the certified HEAD is not a parent of this commit, so the diff the "
            "gate judged is not the diff that was committed",
        )

    def test_blocked_precommit_certifies_nothing(self):
        """A red gate must not leave a certificate behind — that would let the
        very content it rejected be committed with --no-verify unrecorded."""
        h = self.harness(exits={"check-fail-open.py": 1})
        h.write("a.txt", "one\n")
        h.git("add", "-A")
        proc = h.commit("blocked")
        self.assertNotEqual(proc.returncode, 0, "a red gate must block the commit")
        self.assertFalse(
            h.sentinel.exists(), "a blocked pre-commit must not write a sentinel"
        )


# ---------------------------------------------------------------------------
# 2. The bypass is recorded.
# ---------------------------------------------------------------------------
class NoVerifyIsRecorded(GateTestCase):
    def test_no_verify_lands_in_the_ledger(self):
        h = self.harness()
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        proc = h.commit("bypassed", "--no-verify")
        self.assertEqual(proc.returncode, 0, "the bypass itself still succeeds")
        self.assertEqual(h.ran(), [], "no gate can have run under --no-verify")
        self.assertLedgerHas(h, 1, "after `git commit --no-verify`")
        sha = h.git("rev-parse", "HEAD").stdout.strip()
        entry = h.ledger_lines()[0]
        self.assertEqual(
            entry.split("\t")[0], sha, "the ledger must name the commit that was made"
        )
        self.assertEqual(entry.split("\t")[1], "bypassed")

    def test_no_verify_warns_loudly(self):
        h = self.harness()
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        proc = h.commit("bypassed", "--no-verify")
        self.assertIn("UNGATED COMMIT", proc.stderr)
        self.assertIn("bypassed", proc.stderr)

    def test_repeated_bypasses_accumulate(self):
        h = self.harness()
        for i in range(3):
            h.write("a.txt", "ungated %d\n" % i)
            h.git("add", "-A")
            h.commit("bypassed %d" % i, "--no-verify")
        self.assertLedgerHas(h, 3, "after three bypasses")

    def test_cherry_pick_lands_in_the_ledger(self):
        """Replayed content was never inspected HERE, so it is a bypass."""
        h = self.harness()
        h.git("checkout", "-q", "-b", "feat")
        h.write("b.txt", "b\n")
        h.git("add", "-A")
        h.commit("feature")
        self.assertLedgerEmpty(h, "the feature commit itself was gated")
        h.git("checkout", "-q", "main")
        h.git("cherry-pick", "feat")
        self.assertLedgerHas(h, 1, "after cherry-pick")

    def test_rebase_lands_in_the_ledger(self):
        h = self.harness()
        h.git("checkout", "-q", "-b", "topic")
        h.write("t.txt", "t\n")
        h.git("add", "-A")
        h.commit("topic")
        h.git("checkout", "-q", "main")
        h.write("m.txt", "m\n")
        h.git("add", "-A")
        h.commit("main change")
        self.assertLedgerEmpty(h)
        h.git("checkout", "-q", "topic")
        h.git("rebase", "main")
        self.assertLedgerHas(h, 1, "after rebase replayed a commit")


# ---------------------------------------------------------------------------
# 3. Clearing.
# ---------------------------------------------------------------------------
class GreenRunClearsTheLedger(GateTestCase):
    def test_green_precommit_clears_and_reports_the_count(self):
        h = self.harness()
        for i in range(2):
            h.write("a.txt", "ungated %d\n" % i)
            h.git("add", "-A")
            h.commit("bypassed %d" % i, "--no-verify")
        self.assertLedgerHas(h, 2)

        h.write("a.txt", "gated\n")
        h.git("add", "-A")
        proc = h.commit("gated again")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertLedgerEmpty(h, "after a green run")
        self.assertIn("cleared 2 ungated commit(s)", proc.stderr)

    def test_red_precommit_does_not_clear(self):
        """Clearing is supposed to be a side effect of INSPECTION going green.
        A blocked run inspected the content and rejected it; clearing there
        would make a red gate the way to launder a bypass."""
        h = self.harness()
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("bypassed", "--no-verify")
        self.assertLedgerHas(h, 1)

        h.set_stub("check-doc-claims.py", 1)
        h.write("a.txt", "more\n")
        h.git("add", "-A")
        proc = h.commit("attempt")
        self.assertNotEqual(proc.returncode, 0)
        self.assertLedgerHas(h, 1, "a red gate must not clear the ledger")

    def test_no_clear_flag_exists(self):
        """The absence is the design: a `--clear` flag would be a one-command
        bypass of the bypass detector."""
        h = self.harness()
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("bypassed", "--no-verify")
        proc = h.gate_bypass("--clear")
        self.assertNotEqual(proc.returncode, 0)
        self.assertLedgerHas(h, 1, "the ledger must survive an attempt to clear it")


# ---------------------------------------------------------------------------
# 4./5. The ledger consumer's three-valued verdict.
# ---------------------------------------------------------------------------
class GateBypassExitCodes(GateTestCase):
    def test_exit_zero_when_clean(self):
        h = self.harness()
        proc = h.gate_bypass()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertIn("no ungated commit outstanding", proc.stdout)

    def test_exit_one_while_outstanding(self):
        h = self.harness()
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("bypassed", "--no-verify")
        proc = h.gate_bypass()
        self.assertEqual(proc.returncode, 1, proc.stderr)
        self.assertIn("bypassed", proc.stderr)

    def test_json_verdicts_match_the_exit_codes(self):
        h = self.harness()
        proc = h.gate_bypass("--json")
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(json.loads(proc.stdout)["verdict"], "clean")

        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("bypassed", "--no-verify")
        proc = h.gate_bypass("--json")
        self.assertEqual(proc.returncode, 1)
        payload = json.loads(proc.stdout)
        self.assertEqual(payload["verdict"], "outstanding")
        self.assertEqual(len(payload["entries"]), 1)

    def test_undecodable_ledger_exits_two_not_zero(self):
        """The load-bearing one.  An unreadable ledger is NOT an empty ledger:
        collapsing it into exit 0 would make corrupting the file the cheapest
        way to clear it."""
        h = self.harness()
        h.ledger.write_bytes(b"\xff\xfe not utf-8 \x80\x81\n")
        proc = h.gate_bypass()
        self.assertEqual(
            proc.returncode,
            2,
            "an undecodable ledger must be UNDETERMINED, not clean\n"
            "--- stdout ---\n%s\n--- stderr ---\n%s" % (proc.stdout, proc.stderr),
        )
        self.assertIn("UNDETERMINED", proc.stderr)

    def test_undecodable_ledger_json_says_undetermined(self):
        h = self.harness()
        h.ledger.write_bytes(b"\xff\xfe\x80\n")
        proc = h.gate_bypass("--json")
        self.assertEqual(proc.returncode, 2)
        self.assertEqual(json.loads(proc.stdout)["verdict"], "undetermined")

    def test_unreadable_ledger_exits_two(self):
        """The other reachable shape of the same fault: the file exists but the
        process may not open it."""
        if os.geteuid() == 0:  # pragma: no cover - root ignores the mode bits
            self.skipTest("running as root: mode 000 is not enforced")
        h = self.harness()
        h.ledger.write_text("deadbeef\tsomething\n")
        h.ledger.chmod(0)
        self.addCleanup(h.ledger.chmod, stat.S_IRUSR | stat.S_IWUSR)
        proc = h.gate_bypass()
        self.assertEqual(proc.returncode, 2, proc.stderr)

    def test_outside_a_repository_exits_two(self):
        h = self.harness()
        outside = Path(tempfile.mkdtemp(prefix="gate-bypass-nonrepo-")).resolve()
        self.addCleanup(shutil.rmtree, str(outside), True)
        env = dict(h.env)
        env["GIT_CEILING_DIRECTORIES"] = str(outside.parent)
        proc = subprocess.run(
            [_which("python3"), str(h.root / "scripts" / "gate-bypass.py")],
            cwd=str(outside),
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            proc.returncode,
            2,
            "no repository means no verdict, which is not a clean verdict\n%s"
            % proc.stderr,
        )


# ---------------------------------------------------------------------------
# 6. pre-push has teeth.
# ---------------------------------------------------------------------------
class PrePushBlocks(GateTestCase):
    def test_allows_when_clean(self):
        """Control: a hook that always blocked would satisfy the rest."""
        h = self.harness()
        proc = h.pre_push()
        self.assertEqual(
            proc.returncode,
            0,
            "a clean ledger must not block the push\n--- stderr ---\n%s" % proc.stderr,
        )

    def test_blocks_while_outstanding(self):
        h = self.harness()
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("bypassed", "--no-verify")
        proc = h.pre_push()
        self.assertNotEqual(
            proc.returncode, 0, "an outstanding bypass must stop the push"
        )
        self.assertIn("pre-push: blocked", proc.stderr)
        self.assertIn("bypassed", proc.stderr)

    def test_blocks_when_the_checker_is_absent(self):
        """'Cannot check' is not 'clean' — this is the branch that would rot
        into a no-op the moment someone deleted the script."""
        h = self.harness(with_gate_bypass=False)
        proc = h.pre_push()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("cannot check for ungated commits", proc.stderr)

    def test_blocks_on_an_undecodable_ledger(self):
        """exit 2 from the checker must block just as exit 1 does."""
        h = self.harness()
        h.ledger.write_bytes(b"\xff\xfe\x80\n")
        proc = h.pre_push()
        self.assertNotEqual(
            proc.returncode, 0, "undetermined must block, not wave the push through"
        )

    def test_unblocks_after_a_green_run(self):
        """The block must be escapable by doing the right thing, or people will
        route around the whole mechanism."""
        h = self.harness()
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("bypassed", "--no-verify")
        self.assertNotEqual(h.pre_push().returncode, 0)
        h.write("a.txt", "gated\n")
        h.git("add", "-A")
        h.commit("gated")
        self.assertEqual(h.pre_push().returncode, 0, "a green run must unblock")


# ---------------------------------------------------------------------------
# 7. Merges.
# ---------------------------------------------------------------------------
class MergeCommits(GateTestCase):
    def _merge_setup(self, h):
        h.git("checkout", "-q", "-b", "feat")
        h.write("b.txt", "b\n")
        h.git("add", "-A")
        h.commit("feature")
        h.git("checkout", "-q", "main")
        h.write("m.txt", "m\n")
        h.git("add", "-A")
        h.commit("main change")
        h.clear_log()
        self.assertLedgerEmpty(h, "before the merge")

    def test_true_merge_does_not_land_in_the_ledger(self):
        h = self.harness()
        self._merge_setup(h)
        h.git("merge", "--no-ff", "-m", "merge feat", "feat")
        self.assertLedgerEmpty(
            h,
            "a merge commit goes through pre-merge-commit, so it is gated, not "
            "bypassed",
        )

    def test_pre_merge_commit_actually_runs_the_gates(self):
        """The ledger being empty is not enough on its own — it would also be
        empty if nothing ran and nothing recorded.  Observe the gates."""
        h = self.harness()
        self._merge_setup(h)
        h.git("merge", "--no-ff", "-m", "merge feat", "feat")
        self.assertEqual(
            h.ran(), SCANNERS, "pre-merge-commit must run the same gate list"
        )

    def test_a_red_gate_blocks_a_merge_commit(self):
        h = self.harness()
        self._merge_setup(h)
        h.set_stub("check-prompt-injection.py", 1)
        proc = h.git("merge", "--no-ff", "-m", "merge feat", "feat", check=False)
        self.assertNotEqual(
            proc.returncode, 0, "a red gate must stop a merge commit too"
        )

    def test_a_true_merge_consumes_its_certificate(self):
        """`.githooks/post-merge` exists because post-commit does NOT run on a
        merge.  Without it pre-merge-commit's certificate is never consumed and
        survives on disk, where a later `--no-verify` commit of the same tree
        matches it and goes unrecorded.

        Inverted from KNOWN DEFECT `merge_leaves_a_stale_sentinel_behind`; the
        inversion is what proves the fix.
        """
        h = self.harness()
        self._merge_setup(h)
        self.assertFalse(h.sentinel.exists(), "precondition: no certificate yet")

        h.git("merge", "--no-ff", "-m", "merge feat", "feat")
        self.assertFalse(
            h.sentinel.exists(),
            "post-merge must consume the certificate pre-merge-commit wrote",
        )

        # And so the collapse trick no longer works: reduce the merge to an
        # ordinary commit with --no-verify and there is nothing to certify it.
        h.git("reset", "-q", "--soft", "HEAD~1")
        proc = h.commit("ungated re-commit of the merge tree", "--no-verify")
        self.assertEqual(proc.returncode, 0)
        self.assertLedgerHas(h, 1, "the re-commit is a bypass and must be recorded")

    def test_a_fast_forward_merge_is_not_read_as_a_bypass(self):
        """post-merge also runs for a fast-forward, which creates no commit and
        therefore no certificate.  An absent sentinel must not be recorded as a
        bypass there, or every `git pull` would fill the ledger."""
        h = self.harness()
        h.git("checkout", "-q", "-b", "feat")
        h.write("b.txt", "b\n")
        h.git("add", "-A")
        h.commit("feature")
        h.git("checkout", "-q", "main")
        h.clear_log()
        self.assertLedgerEmpty(h, "before the merge")

        h.git("merge", "--ff-only", "feat")
        self.assertLedgerEmpty(h, "a fast-forward creates no commit to certify")

    def test_merge_with_no_verify_is_recorded_and_blocks_the_push(self):
        """`git merge --no-ff --no-verify` skips pre-merge-commit, so the merge
        is genuinely ungated — that cannot be prevented.  post-merge survives
        the flag, so it can still be made impossible to hide.

        Inverted from KNOWN DEFECT `merge_with_no_verify_is_never_recorded`.
        """
        h = self.harness()
        self._merge_setup(h)

        h.git("merge", "--no-ff", "--no-verify", "-m", "ungated merge", "feat")
        self.assertEqual(h.ran(), [], "precondition: no gate ran")
        self.assertLedgerHas(h, 1, "the ungated merge must reach the ledger")
        self.assertNotEqual(
            h.pre_push().returncode, 0, "and pre-push must refuse to push it"
        )


# ---------------------------------------------------------------------------
# 8. The sentinel is per-worktree.
# ---------------------------------------------------------------------------
class SentinelIsPerWorktree(GateTestCase):
    def _add_worktree(self, h):
        other = h.root.parent / (h.root.name + "-wt")
        self.addCleanup(shutil.rmtree, str(other), True)
        h.git("worktree", "add", "-q", "-b", "other", str(other))
        return other

    def test_sentinels_live_at_distinct_paths(self):
        h = self.harness()
        other = self._add_worktree(h)
        a = h.git("rev-parse", "--git-dir").stdout.strip()
        b = h.git("rev-parse", "--git-dir", cwd=other).stdout.strip()
        self.assertNotEqual(a, b, "linked worktrees must have distinct git dirs")
        # `--git-common-dir` can be relative; resolve each against ITS OWN
        # worktree, not against this process's cwd.
        common_a = (h.root / h.git("rev-parse", "--git-common-dir").stdout.strip()).resolve()
        common_b = (
            other / h.git("rev-parse", "--git-common-dir", cwd=other).stdout.strip()
        ).resolve()
        self.assertEqual(
            common_a, common_b, "the LEDGER, by contrast, is shared"
        )

    def test_one_worktree_cannot_certify_anothers_identical_tree(self):
        """The sharp version.  Both worktrees stage byte-identical content from
        the same base, so their index trees hash the SAME.  If the sentinel were
        repo-wide, worktree A's green run would certify worktree B's ungated
        commit and the ledger would stay empty."""
        h = self.harness()
        other = self._add_worktree(h)

        h.write("shared.txt", "identical\n")
        h.git("add", "-A")
        h.write("shared.txt", "identical\n", cwd=other)
        h.git("add", "-A", cwd=other)

        tree_a = h.git("write-tree").stdout.strip()
        tree_b = h.git("write-tree", cwd=other).stdout.strip()
        self.assertEqual(
            tree_a,
            tree_b,
            "test precondition: the two indexes must hash to the same tree, or "
            "this proves nothing about sharing",
        )

        # A goes green and certifies the tree, but does not commit.
        proc = h.pre_commit()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(h.sentinel.exists())

        # B commits that same tree WITHOUT the gate.
        proc = h.commit("ungated in the other worktree", "--no-verify", cwd=other)
        self.assertEqual(proc.returncode, 0, proc.stderr)

        self.assertLedgerHas(
            h,
            1,
            "worktree B's ungated commit must be recorded even though worktree "
            "A holds a green certificate for the identical tree",
        )
        self.assertTrue(
            h.sentinel.exists(),
            "worktree B must not consume worktree A's certificate either",
        )


# ---------------------------------------------------------------------------
# 9. deny-no-verify.py — refusal at the Bash call.
# ---------------------------------------------------------------------------
class DenyNoVerify(unittest.TestCase):
    SCRIPT = _SCRIPTS_DIR / "deny-no-verify.py"

    def run_hook(self, payload):
        return subprocess.run(
            [_which("python3"), str(self.SCRIPT)],
            input=payload if isinstance(payload, str) else json.dumps(payload),
            capture_output=True,
            text=True,
        )

    def bash(self, command):
        return self.run_hook({"tool_name": "Bash", "tool_input": {"command": command}})

    def assertDenied(self, command):
        proc = self.bash(command)
        self.assertEqual(
            proc.returncode,
            2,
            "must DENY: %r\n--- stderr ---\n%s" % (command, proc.stderr),
        )
        self.assertIn("Refused", proc.stderr)

    def assertAllowed(self, command):
        proc = self.bash(command)
        self.assertEqual(
            proc.returncode,
            0,
            "must ALLOW: %r\n--- stderr ---\n%s" % (command, proc.stderr),
        )

    def test_denies_the_plain_bypasses(self):
        for command in (
            "git commit --no-verify -m x",
            "git commit -n -m x",
            "git push --no-verify",
            "git push -n origin main",
            "git merge --no-verify --no-ff feat",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_denies_through_a_path_or_env_prefix(self):
        for command in (
            "/usr/bin/git commit --no-verify -m x",
            "FOO=1 git commit --no-verify -m x",
            "FOO=1 BAR=2 /usr/local/bin/git commit --no-verify",
            "x && git commit --no-verify -m x",
            "true; git commit --no-verify",
            "make build || git commit --no-verify",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_denies_behind_a_valued_global_option(self):
        """`git -C <path> commit --no-verify` puts a NON-flag token before the
        subcommand.  Reading the first non-flag token as the subcommand resolves
        it to the path, finds it unguarded, and lets the bypass through."""
        for command in (
            "git -C /tmp/repo commit --no-verify -m x",
            "git -c user.name=x commit --no-verify",
            "git --work-tree /tmp commit --no-verify",
            "git --git-dir /tmp/r/.git commit --no-verify",
            "git --namespace ns push --no-verify",
            "git -C /tmp/repo -c a=b commit -n",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_allows_ordinary_commands(self):
        for command in (
            "git commit -m x",
            "git commit -m 'x --no-verify y'",
            'git commit -m "note: do not use --no-verify here"',
            "git log -n 5",
            "git status -n",
            "echo --no-verify",
            "git -C /tmp status",
            "git -c user.name=x log -n 3",
            "grep -rn 'no-verify' scripts/",
            "cargo test -p condukt",
        ):
            with self.subTest(command=command):
                self.assertAllowed(command)

    def test_the_short_flag_is_only_a_bypass_where_git_honours_it(self):
        """`-n` is --no-verify for commit/push and something else entirely for
        everyone else.  Treating it as a bypass everywhere would deny `git log
        -n 5`, which is how a guard like this gets turned off."""
        self.assertDenied("git commit -n")
        self.assertDenied("git push -n")
        # `git merge -n` is --no-stat, not --no-verify.
        self.assertAllowed("git merge -n feat")

    def test_non_bash_tools_are_allowed(self):
        for tool in ("Read", "Edit", "Write", "Glob"):
            with self.subTest(tool=tool):
                proc = self.run_hook(
                    {"tool_name": tool, "tool_input": {"command": "git commit -n"}}
                )
                self.assertEqual(proc.returncode, 0)

    def test_unparseable_payloads_are_allowed(self):
        for payload in ("", "not json", "{", '{"tool_name": "Bash"}'):
            with self.subTest(payload=payload):
                proc = self.run_hook(payload)
                self.assertEqual(
                    proc.returncode, 0, "stderr:\n%s" % proc.stderr
                )

    def test_non_string_command_is_allowed(self):
        proc = self.run_hook(
            {"tool_name": "Bash", "tool_input": {"command": ["git", "commit", "-n"]}}
        )
        self.assertEqual(proc.returncode, 0)

    def test_unbalanced_quotes_do_not_crash(self):
        proc = self.bash("git commit -m 'unterminated")
        self.assertIn(proc.returncode, (0, 2))
        self.assertNotIn("Traceback", proc.stderr)


# ---------------------------------------------------------------------------
# KNOWN DEFECTS — observed, wrong, and pinned here so they cannot be forgotten.
#
# These tests assert the CURRENT, DEFECTIVE behaviour.  They are green today by
# construction; each one must be INVERTED when its defect is fixed, and the
# inversion is what proves the fix.  Nothing here is an endorsement.
# ---------------------------------------------------------------------------
class KnownDefects(GateTestCase):
    def test_DEFECT_stale_sentinel_certifies_a_later_ungated_commit(self):
        """DEFECT (severity: medium — reduced, not closed, by the HEAD binding).
        pre-commit writes the certificate, post-commit consumes it — but if a
        green pre-commit run is NOT followed by a commit, the certificate
        survives on disk with no expiry.  A later `--no-verify` commit of the
        same (tree, parent) pair matches it and is never recorded.

        Binding HEAD into the certificate closed the variant where HEAD moves in
        between (pinned above by
        `test_certificate_does_not_survive_head_moving`).  It does NOT close the
        common case where HEAD has not moved, which is what this reproduces:
        a commit aborted at the message editor — an everyday accident, since the
        gates run BEFORE the editor opens — leaves a live certificate, and the
        very next `--no-verify` commit walks through on it.

        The content is arguably the same content the gate judged, so this is not
        by itself an unexamined-content hole.  What it IS: the ledger's stated
        meaning ("this commit did not go through the gate") becomes false, and
        the certificate has no lifetime bound at all — nothing stops it being
        reused an hour or a day later, by a different operation, as long as the
        pair still matches.

        Fix direction: give the certificate a lifetime (pid of the git process,
        or an mtime bound), so it can only certify the commit it was written for.
        """
        h = self.harness()
        h.write("a.txt", "content\n")
        h.git("add", "-A")

        env = dict(h.env, GIT_EDITOR="true")  # empty message -> commit aborts
        aborted = subprocess.run(
            [_which("git"), "commit"],
            cwd=str(h.root),
            env=env,
            capture_output=True,
            text=True,
        )
        self.assertNotEqual(aborted.returncode, 0, "precondition: the commit aborted")
        self.assertEqual(h.ran(), SCANNERS, "precondition: the gates did run")
        self.assertTrue(
            h.sentinel.exists(),
            "precondition: a certificate survived a commit that never happened",
        )

        proc = h.commit("ungated, but certified by a stale sentinel", "--no-verify")
        self.assertEqual(proc.returncode, 0)
        self.assertEqual(
            h.ledger_lines(),
            [],
            "DEFECT PINNED: the ungated commit was NOT recorded. When this is "
            "fixed, this assertion must become assertLedgerHas(h, 1).",
        )
        self.assertNotIn("UNGATED COMMIT", proc.stderr)

    def test_DEFECT_a_green_run_clears_content_it_never_inspected(self):
        """DEFECT (severity: high).  The ledger is cleared by a green run over
        the CURRENT index.  The bypassed COMMIT's content is never re-examined,
        and for any diff-based gate the bypass actively helps: once the change
        is in HEAD, a gate that compares against HEAD sees nothing to complain
        about.

        Modelled here with a stub that is red exactly while a file differs from
        HEAD — the shape of scripts/check-version-bumped.py, whose --base
        defaults to HEAD.  Sequence: the gate is red, the author bypasses it,
        and the very next commit goes green *because* the bypass moved the
        violation into HEAD.  The ledger clears; the ungated commit is now
        pushable and was never inspected.
        """
        h = self.harness()
        # A stub that fails while `guarded.txt` differs from HEAD.
        (h.root / "scripts" / "check-version-bumped.py").write_text(
            "#!/usr/bin/env python3\n"
            "import os, subprocess, sys\n"
            "log = os.environ.get('GATE_TEST_LOG')\n"
            "if log:\n"
            "    open(log, 'a').write('check-version-bumped.py\\n')\n"
            "d = subprocess.run(['git', 'diff', '--cached', '--name-only', 'HEAD',\n"
            "                    '--', 'guarded.txt'], capture_output=True, text=True)\n"
            "sys.exit(1 if d.stdout.strip() else 0)\n"
        )
        (h.root / "scripts" / "check-version-bumped.py").chmod(0o755)

        h.write("guarded.txt", "violation\n")
        h.git("add", "-A")
        blocked = h.commit("should be blocked")
        self.assertNotEqual(blocked.returncode, 0, "precondition: the gate is red")

        h.commit("bypassed the red gate", "--no-verify")
        self.assertLedgerHas(h, 1, "precondition: the bypass was recorded")

        # Any subsequent commit now goes green: guarded.txt no longer differs
        # from HEAD, because the bypass put it there.
        h.write("unrelated.txt", "harmless\n")
        h.git("add", "-A")
        proc = h.commit("unrelated, and green")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(
            h.ledger_lines(),
            [],
            "DEFECT PINNED: the ledger cleared without the bypassed content "
            "ever being inspected — the bypass is what made the gate green.",
        )
        self.assertEqual(h.pre_push().returncode, 0, "DEFECT PINNED: and it pushes")

    def test_DEFECT_deny_hook_misses_an_abbreviated_flag(self):
        """DEFECT (severity: medium).  git accepts any unambiguous prefix of a
        long option, so `git commit --no-verif` is `--no-verify`.  The hook
        compares against the exact string and allows it.

        Confirmed against real git in the harness below: the abbreviated form
        produces a commit that post-commit records, i.e. git honoured it.
        """
        proc = subprocess.run(
            [_which("python3"), str(_SCRIPTS_DIR / "deny-no-verify.py")],
            input=json.dumps(
                {
                    "tool_name": "Bash",
                    "tool_input": {"command": "git commit --no-verif -m x"},
                }
            ),
            capture_output=True,
            text=True,
        )
        self.assertEqual(
            proc.returncode,
            0,
            "DEFECT PINNED: the abbreviation is allowed. When fixed, this must "
            "become assertEqual(..., 2).",
        )

        # ... and git really does treat it as the bypass.
        h = self.harness()
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("abbreviated bypass", "--no-verif")
        self.assertEqual(h.ran(), [], "git honoured the abbreviation: no gate ran")
        self.assertLedgerHas(h, 1, "the ledger did catch what the deny hook missed")

    def test_DEFECT_deny_hook_misses_a_nested_shell(self):
        """DEFECT (severity: medium).  The segment splitter knows `&&`, `||`,
        `;`, `|` and newlines, and nothing about quoting or nesting.  Anything
        that puts the bypass inside another word survives.

        Not fatal on its own — the ledger still records the resulting commit —
        but the module docstring claims this hook refuses the bypass "outright",
        and for these forms it does not.
        """
        for command in (
            "bash -c 'git commit --no-verify -m x'",
            "sh -c \"git commit -n\"",
            "(git commit --no-verify -m x)",
            "echo $(git commit --no-verify -m x)",
        ):
            with self.subTest(command=command):
                proc = subprocess.run(
                    [_which("python3"), str(_SCRIPTS_DIR / "deny-no-verify.py")],
                    input=json.dumps(
                        {"tool_name": "Bash", "tool_input": {"command": command}}
                    ),
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(
                    proc.returncode, 0, "DEFECT PINNED: %r is allowed" % command
                )

    def test_DEFECT_documented_status_flag_does_not_exist(self):
        """DEFECT (severity: low, but it is the docstring-lies class).
        `.githooks/post-commit` tells the reader to run

            python3 scripts/gate-bypass.py --status

        and `scripts/gate-bypass.py`'s own header says "The teeth are in
        `scripts/gate-bypass.py --status`".  argparse has no such option; the
        command exits 2 with a usage error.  A reader who follows the printed
        instruction sees UNDETERMINED-coloured failure and cannot tell it apart
        from a real one, because 2 is also the undetermined code.
        """
        h = self.harness()
        proc = h.gate_bypass("--status")
        self.assertEqual(proc.returncode, 2)
        self.assertIn("unrecognized arguments: --status", proc.stderr)
        self.assertNotIn(
            "no ungated commit outstanding",
            proc.stdout,
            "DEFECT PINNED: the documented invocation does not work",
        )

        hook_text = (_HOOKS_DIR / "post-commit").read_text()
        self.assertIn(
            "gate-bypass.py --status",
            hook_text,
            "the hook still prints an invocation that does not exist",
        )

    def test_DEFECT_a_ledger_line_with_no_commit_id_reads_as_a_commit(self):
        """DEFECT (severity: low, but it is a dead fail-closed branch).
        `scripts/gate-bypass.py` has an explicit guard:

            sha, _, subject = line.partition("\t")
            if not sha:
                raise Undetermined(... "has no commit id")

        It can never fire.  Two lines above, `line = line.strip()` removes the
        leading tab, so a line whose commit-id field is empty becomes a line
        whose commit id is the SUBJECT.  The malformed entry is then reported as
        a perfectly ordinary outstanding commit — exit 1, not exit 2 — and the
        printed "commit id" is the first twelve characters of the subject.

        Not exploitable on its own (1 blocks as hard as 2 does), but it is a
        guard the author believes exists and does not, and it mis-identifies the
        commit in the message a human is meant to act on.
        """
        h = self.harness()
        h.ledger.write_text("\tsubject with no sha\n")
        proc = h.gate_bypass()
        self.assertEqual(
            proc.returncode,
            1,
            "DEFECT PINNED: reported as an ordinary entry, not as undetermined. "
            "When fixed, this must become assertEqual(..., 2).",
        )
        self.assertIn("subject with", proc.stderr, "the subject is printed AS the sha")

    def test_DEFECT_a_non_dict_json_payload_crashes_the_deny_hook(self):
        """DEFECT (severity: low).  The module docstring says an unparseable
        payload exits 0.  A payload that PARSES but is not an object — `[]`,
        `"x"`, `3` — reaches `payload.get(...)` and raises AttributeError, so
        the hook dies with an unhandled traceback and exit 1.

        Exit 1 is not a deny, so the bypass is not blocked either way; the cost
        is a stack trace on every Bash call for as long as the malformed shape
        persists, and a hook whose stated fail-open contract it does not meet.
        """
        for payload in ("[]", '"a string"', "3", "null"):
            with self.subTest(payload=payload):
                proc = subprocess.run(
                    [_which("python3"), str(_SCRIPTS_DIR / "deny-no-verify.py")],
                    input=payload,
                    capture_output=True,
                    text=True,
                )
                self.assertEqual(
                    proc.returncode, 1, "DEFECT PINNED: %r" % payload
                )
                self.assertIn("Traceback", proc.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
