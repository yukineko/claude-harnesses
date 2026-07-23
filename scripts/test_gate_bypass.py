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
            # `git push -n` was here and was WRONG — for push, `-n` is
            # --dry-run, not --no-verify.  See
            # test_the_short_flag_is_only_a_bypass_where_git_honours_it below,
            # which now carries the `git push -h` evidence.  Replaced with the
            # spellings push actually honours rather than dropped.
            "git push --no-verify origin main",
            "git push --no-verif origin main",
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
        """`-n` is --no-verify for COMMIT ONLY.  Every other subcommand spells
        `-n` differently, so treating it as a bypass everywhere denies safe
        commands, which is how a guard like this gets switched off.

        This test previously asserted `assertDenied("git push -n")` and was
        WRONG — it pinned a falsehood as if it were the specification, which is
        exactly the failure mode CLAUDE.md 2(a) warns about.  The evidence is
        recorded here so the next reader does not have to re-derive it.  From
        the git on this machine:

            $ git commit -h
                -n, --no-verify       bypass pre-commit and commit-msg hooks
            $ git push -h
                -n, --dry-run         dry run
                    --no-verify       bypass pre-push hook
            $ git merge -h
                -n                    do not show a diffstat at the end
                    --no-verify       bypass pre-merge-commit and commit-msg hooks

        So: commit honours the short form; push and merge are guarded for the
        LONG flag only.  `git push -n` is a dry run — it contacts nothing and
        skips nothing — and refusing it was a false positive on the safest
        command in the set.
        """
        self.assertDenied("git commit -n")
        self.assertDenied("git commit -n -m x")
        # push: -n is --dry-run.  The long flag is still a bypass.
        self.assertAllowed("git push -n")
        self.assertAllowed("git push -n origin main")
        self.assertAllowed("git push --dry-run origin main")
        self.assertDenied("git push --no-verify origin main")
        # merge: -n is --no-stat.  The long flag is still a bypass.
        self.assertAllowed("git merge -n feat")
        self.assertDenied("git merge --no-verify feat")

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

    def test_deny_hook_catches_an_abbreviated_flag(self):
        """FIXED in 5453b4bc — this test is the INVERSION of the defect it used
        to pin, per the contract in this class's header.

        git accepts any unambiguous prefix of a long option, so
        `git commit --no-verif` is `--no-verify`.  The hook used to compare
        against the exact string and allowed it.  It now accepts a token as the
        flag when the token starts with `--no-v` and is a prefix of the full
        spelling.

        The harness below is kept unchanged: it is the independent evidence
        that git really does honour the abbreviation, which is what makes the
        assertion above worth making.
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
            2,
            "the abbreviation must be refused; stderr:\n%s" % proc.stderr,
        )

        # ... and git really does treat it as the bypass.
        h = self.harness()
        h.write("a.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("abbreviated bypass", "--no-verif")
        self.assertEqual(h.ran(), [], "git honoured the abbreviation: no gate ran")
        self.assertLedgerHas(h, 1, "the ledger did catch what the deny hook missed")

    def _deny_hook(self, command):
        return subprocess.run(
            [_which("python3"), str(_SCRIPTS_DIR / "deny-no-verify.py")],
            input=json.dumps(
                {"tool_name": "Bash", "tool_input": {"command": command}}
            ),
            capture_output=True,
            text=True,
        )

    def test_deny_hook_catches_a_subshell(self):
        """PARTIALLY FIXED in 5453b4bc — the subshell half only.

        This used to be one test over four commands, two of which are now fixed
        and two of which are not.  Splitting it is deliberate: a single test
        covering both would be GREEN on the fixed half and RED on the broken
        half, and whichever way it was written, one half would be certifying the
        other.  The wrapper half keeps its DEFECT name below.

        `(` and `)` are now in SEPARATORS, so a subshell ends the segment
        instead of shifting `git` out of argv[0].
        """
        for command in (
            "(git commit --no-verify -m x)",
            "( git commit --no-verify -m x )",
            "echo $(git commit --no-verify -m x)",
        ):
            with self.subTest(command=command):
                proc = self._deny_hook(command)
                self.assertEqual(
                    proc.returncode, 2, "%r must be refused" % command
                )

    def test_DEFECT_deny_hook_misses_an_interpreter_wrapper(self):
        """DEFECT (severity: medium) — the half of the old nested-shell defect
        that 5453b4bc did NOT fix, kept separate so the fixed half cannot
        certify it.

        `is_bypass` inspects argv[0] of each segment.  An interpreter takes the
        whole command as a single quoted ARGUMENT, so `git` is never argv[0] and
        the segment is dismissed.  Confirmed reaching real git.

        Not fatal on its own — the ledger still records the resulting commit —
        but the module docstring used to claim this hook refuses the bypass
        "outright", and for these forms it does not.  The docstring now names
        this hole explicitly instead.
        """
        for command in (
            "bash -c 'git commit --no-verify -m x'",
            "sh -c \"git commit -n\"",
            "eval 'git commit --no-verify -m x'",
        ):
            with self.subTest(command=command):
                proc = self._deny_hook(command)
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

    def test_DEFECT_the_backstop_does_not_hold_for_a_hooksPath_bypass(self):
        """DEFECT (severity: HIGH — it falsifies the stated justification for a
        deliberate fail-open, which makes it a CLAUDE.md 4 problem and not just
        a missing feature).

        `scripts/deny-no-verify.py` exits 0 on an unparseable PAYLOAD, and its
        docstring argues that this is acceptable because the hook is not the
        gate:

            "The gate is `.githooks/*` plus the bypass ledger, which run
             regardless of what happens here."

        "run regardless" is the load-bearing claim, and it is FALSE for the
        `-c core.hooksPath=` bypass — the one the deny hook itself classifies as
        "any subcommand", and whose own code comment already says it disables
        post-commit too.  Nobody had constructed the case.  Constructed here,
        observed in this harness:

            git commit -m plain                    ledger 0, pre-push exit 0
            git commit --no-verify -m ungated      ledger 1, pre-push exit 1  <- backstop works
            git -c core.hooksPath=/dev/null commit ledger 0, pre-push exit 0  <- backstop absent

        The chain is post-commit -> ledger -> pre-push.  Overriding hooksPath
        breaks it at link one: post-commit never runs, so nothing is recorded,
        so pre-push finds an empty ledger and ALLOWS THE PUSH.  The ungated
        commit leaves the machine.

        The consequence for the docstring is specific: for this bypass class,
        deny-no-verify.py is not a redundant convenience in front of a durable
        backstop — it is the ONLY control.  A fail-open in it is therefore
        unconditional, and the paragraph that justifies the payload exemption by
        pointing at `.githooks/*` does not apply here.  Either the backstop must
        be extended to cover it, or that paragraph must stop claiming coverage
        it does not have.

        This test asserts the CURRENT, WRONG behaviour.  When the backstop is
        extended, the ledger assertion becomes `1` and pre-push becomes
        non-zero, and this test is the proof it worked.
        """
        h = self.harness()
        h.write("c.txt", "ungated via hooksPath\n")
        h.git("add", "-A")
        proc = h.git("-c", "core.hooksPath=/dev/null", "commit", "-q",
                     "-m", "hooks off", check=False)
        self.assertEqual(proc.returncode, 0, "the commit itself must succeed")
        self.assertEqual(h.ran(), [], "no gate ran: hooksPath pointed away")
        self.assertEqual(
            h.ledger_lines(),
            [],
            "DEFECT PINNED: post-commit never ran, so the bypass was NOT "
            "recorded. When the backstop covers this, expect 1 entry.",
        )
        self.assertEqual(
            h.pre_push().returncode,
            0,
            "DEFECT PINNED: pre-push allows the push because the ledger it "
            "consults is empty. When fixed this must become non-zero.",
        )

    def test_the_backstop_DOES_hold_for_the_plain_no_verify_bypass(self):
        """Control for the test above, and the reason it is a finding rather
        than a complaint: the backstop is real, it works, and it works for the
        bypass it was written for.  Without this, "the backstop does not hold"
        could equally mean the harness never wired it up."""
        h = self.harness()
        h.write("b.txt", "ungated\n")
        h.git("add", "-A")
        h.commit("ungated", "--no-verify")
        self.assertEqual(h.ran(), [], "no gate ran: --no-verify skipped them")
        self.assertLedgerHas(h, 1, "post-commit recorded the ungated commit")
        self.assertNotEqual(
            h.pre_push().returncode, 0, "pre-push must refuse to send it"
        )

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


# ---------------------------------------------------------------------------
# CERTIFICATION of commit 365941f8 (`deny-no-verify.py`: tokenize, then split on
# separator TOKENS).  Written by a verifier who did not write the fix.
#
# Every test in this class was OBSERVED RED before being accepted: each one was
# run against `git show 365941f8^:scripts/deny-no-verify.py` (the pre-fix file)
# and failed there.  A test nobody has seen fail proves nothing (CLAUDE.md 2).
#
# The pre-fix file is not vendored into the repo; DENY_HOOK_UNDER_TEST lets the
# same class be pointed at an arbitrary copy so the RED run is reproducible:
#
#     git show 365941f8^:scripts/deny-no-verify.py > /tmp/prefix.py
#     DENY_HOOK_UNDER_TEST=/tmp/prefix.py python3 -m unittest \
#         scripts.test_gate_bypass.QuotedSeparatorsDoNotDisableTheRefusal
# ---------------------------------------------------------------------------
class QuotedSeparatorsDoNotDisableTheRefusal(DenyNoVerify):
    """The regression the fix was for: a separator INSIDE a quoted argument.

    Splitting the raw string on `;`/`|` before tokenizing cut through the commit
    message, left the first fragment with an unbalanced quote, and the
    `except ValueError: return None` read as "not a bypass".  One punctuation
    character in a message turned the whole refusal off.
    """

    SCRIPT = Path(os.environ.get("DENY_HOOK_UNDER_TEST",
                                 str(_SCRIPTS_DIR / "deny-no-verify.py")))

    def test_a_separator_inside_a_double_quoted_message_still_denies(self):
        for command in (
            'git commit --no-verify -m "a; b"',
            'git commit --no-verify -m "a | b"',
            'git commit --no-verify -m "a && b"',
            'git commit --no-verify -m "a || b"',
            'git commit --no-verify -m "a & b"',
            'git commit --no-verify -m "fix: a; then b | c && d"',
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_a_separator_inside_a_single_quoted_message_still_denies(self):
        for command in (
            "git commit --no-verify -m 'a; b'",
            "git commit --no-verify -m 'a | b'",
            "git push --no-verify -m 'a && b'",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_the_short_form_is_not_disabled_by_a_quoted_separator(self):
        """`-n` took the same path as `--no-verify`, so it had the same hole."""
        for command in (
            'git commit -n -m "a; b"',
            "git commit -n -m 'a | b'",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_ansi_c_quoting_and_backslash_escapes_still_deny(self):
        for command in (
            "git commit --no-verify -m $'a;b'",
            "git commit --no-verify -m a\\;b",
            "git commit --no-verify -m a\\|b",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_a_newline_inside_a_quoted_message_still_denies(self):
        """A multi-paragraph commit message is the ordinary case, not an exotic
        one.  The old regex split on `\\n` too, so this had the hole as well."""
        self.assertDenied('git commit --no-verify -m "subject\n\nbody"')

    def test_real_separators_between_commands_still_split(self):
        """Control for the four above: the fix must not have bought quoted-
        separator safety by giving up on separators that really are separators.
        Without this, a hook that never splits at all would pass this class."""
        for command in (
            "cargo test; git commit --no-verify -m x",
            "cargo test && git commit --no-verify -m x",
            "cargo test || git commit --no-verify -m x",
            "echo x | git commit --no-verify -F -",
            "git commit --no-verify -m x &",
            "sleep 1 & git commit --no-verify -m x",
            "cargo test |& git commit --no-verify -m x",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_an_untokenizable_command_carrying_a_marker_is_refused(self):
        """The cannot-determine branch.  Exit code asserted EXACTLY, because
        the point of the branch is which of 0 and 2 it resolves to."""
        for command in (
            'git commit --no-verify -m "unbalanced',
            "git commit -n -m 'unbalanced",
            'git push --no-verify --repo "unbalanced',
        ):
            with self.subTest(command=command):
                proc = self.bash(command)
                self.assertEqual(
                    proc.returncode,
                    2,
                    "a command that would not tokenize and carries a bypass "
                    "marker must resolve to REFUSE, not to permit: %r\n%s"
                    % (command, proc.stderr),
                )
                self.assertIn("could not be parsed", proc.stderr)

    def test_an_untokenizable_command_with_no_marker_is_allowed(self):
        """The other half of the same branch, asserted just as exactly.  An
        unbalanced quote in an unrelated command must not take the session down.
        """
        for command in (
            'echo "unbalanced',
            "grep 'foo bar",
            'python3 -c "print(1)\'',
        ):
            with self.subTest(command=command):
                proc = self.bash(command)
                self.assertEqual(
                    proc.returncode,
                    0,
                    "must ALLOW an unparseable non-bypass: %r\n%s"
                    % (command, proc.stderr),
                )

    def test_the_exact_command_from_the_vacuous_crash_test_is_allowed(self):
        """Replacement for `test_unbalanced_quotes_do_not_crash` (line ~902),
        which asserts `assertIn(proc.returncode, (0, 2))`.

        That assertion accepts ALLOW and DENY alike.  Measured against three
        mutant hooks — always-allow, always-deny, and deny-every-tool — it
        survives all three, so it discriminates only "crashed (exit 1)" from
        "did not crash".  The semicolon bug changed the hook from 2 to 0 and
        that test could not have gone red on it.  It is not weakened or removed
        here; this is the missing exact assertion, added alongside it.

        `git commit -m 'unterminated` carries neither marker, so the
        cannot-determine branch must resolve to ALLOW — exactly 0, not "0 or 2".
        """
        proc = self.bash("git commit -m 'unterminated")
        self.assertEqual(
            proc.returncode,
            0,
            "unparseable and no bypass marker => allow\n%s" % proc.stderr,
        )
        self.assertNotIn("Traceback", proc.stderr)

    def test_ordinary_work_is_still_allowed(self):
        """A refusal that fires on ordinary commands gets switched off, so the
        false-positive side is part of the certification, not an afterthought."""
        for command in (
            "git commit -m 'a; b'",
            'git commit -m "wip: a | b"',
            "git log -n 5 | head -n 3",
            "git status --short; cargo test",
            "grep -rn -- '--no-verify' scripts/",
            'echo "never pass --no-verify; it is recorded"',
            "cargo clippy --workspace && cargo fmt --all",
        ):
            with self.subTest(command=command):
                self.assertAllowed(command)


# ---------------------------------------------------------------------------
# KNOWN DEFECTS in deny-no-verify.py, found while certifying 365941f8.
#
# Same contract as the KnownDefects class above: each test asserts the CURRENT,
# WRONG behaviour, is green by construction, and must be INVERTED when its
# defect is fixed.  Nothing here is an endorsement.
#
# Each `-> git exit 0, hook never ran` note below was CONFIRMED against real git
# (2.x, macOS) in a throwaway repo whose pre-commit hook prints and exits 1: the
# commit landed and the hook never printed.  These are reproduced bypasses, not
# suspicions.
# ---------------------------------------------------------------------------


# ---------------------------------------------------------------------------
# INVERSIONS.  Each test below was a `test_DEFECT_*` in this file until
# 5453b4bc closed its defect; the DEFECT contract says the inversion is what
# proves the fix, so these now assert the correct behaviour going forward.
#
# Every one was OBSERVED RED against the immediately preceding production file
# (`git show f53f15b3:scripts/deny-no-verify.py`) and green on HEAD.  Pointing
# DENY_HOOK_UNDER_TEST at that blob reproduces the red run.
# ---------------------------------------------------------------------------
class FixesLandedIn5453bc4(DenyNoVerify):
    SCRIPT = Path(os.environ.get("DENY_HOOK_UNDER_TEST",
                                 str(_SCRIPTS_DIR / "deny-no-verify.py")))

    def test_a_newline_between_commands_splits_them(self):
        """The regression the previous fix introduced, now closed.

        `punctuation_chars` does not make shlex emit a newline — a newline is
        whitespace — so `"\\n"` sat in SEPARATORS as an entry the lexer could
        never produce, and two lines FUSED into one segment whose argv[0] was
        `cargo`.  A multi-line command string is the ordinary shape an agent
        submits, so that hole was wider than the semicolon bug it shipped with.

        Now: `\\n`/`\\r` are moved out of `lexer.whitespace` into
        `punctuation_chars`, and CRLF arrives as one `"\\r\\n"` token matched by
        NEWLINE_TOKEN rather than by set membership.
        """
        for command in (
            "cargo test\ngit commit --no-verify -m x",
            "cd /repo\ngit commit -n -m x",
            "cargo fmt\ncargo test\ngit push --no-verify",
            "cargo test\r\ngit commit --no-verify -m x",
            "cargo test\rgit commit --no-verify -m x",
            "\ngit commit --no-verify -m x\n",
            "cargo test\n\n\ngit commit --no-verify -m x",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_a_newline_does_not_split_inside_a_quoted_message(self):
        """The other side of that change, and the one that could have been
        broken by it: a multi-paragraph commit message is ONE argument.  If the
        newline split inside quotes, a message that happens to contain a line
        starting with `git commit` would be read as a second command.
        """
        self.assertAllowed('git commit -m "subject\n\nbody"')
        self.assertAllowed('git commit -m "fix\ngit commit --no-verify was wrong"')
        self.assertDenied('git commit --no-verify -m "subject\n\nbody"')

    def test_ordinary_multi_line_work_is_still_allowed(self):
        """Guard against buying the fix with a blanket refusal on multi-line
        input.  Without this, a hook that denied every newline would pass the
        test above."""
        for command in (
            "cargo fmt\ncargo test",
            "cd /repo\ncargo build\ngit status",
            "git add x\ngit commit -m ok",
        ):
            with self.subTest(command=command):
                self.assertAllowed(command)

    def test_whitespace_only_commands_do_not_crash_or_deny(self):
        """`split_commands` returns `[]` for these.  An empty segment list is
        the one case where "nothing to judge" really is nothing to judge, but it
        is also exactly the shape that a `for seg in segments` loop silently
        treats as clean, so it is asserted rather than assumed."""
        for command in ("", "   ", "\n", "\r\n", "\n\n\n"):
            with self.subTest(command=repr(command)):
                proc = self.bash(command)
                self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assertNotIn("Traceback", proc.stderr)

    def test_a_glued_short_option_cluster_is_recognised(self):
        """`-nm`, `-anm`, `-nqm` are `--no-verify` as much as a bare `-n` is;
        matching only the whole token `-n` missed all of them.  Confirmed
        against real git: each committed with exit 0 and the hook never ran."""
        for command in ("git commit -nm ok", "git commit -anm ok",
                        "git commit -nqm ok", "git commit -nm 'a; b'"):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_a_cluster_without_n_is_not_a_bypass(self):
        """The boundary of the cluster rule.  No `git commit` short option
        other than `-n` contains the letter `n`, which is why the rule is safe;
        the clusters an author actually types must keep working."""
        for command in ("git commit -am ok", "git commit -av",
                        "git commit -sm ok", "git commit -qam ok"):
            with self.subTest(command=command):
                self.assertAllowed(command)

    def test_an_abbreviated_long_option_is_recognised(self):
        """git accepts any UNAMBIGUOUS abbreviation of a long option.
        Confirmed against real git: `--no-verif` and `--no-veri` both committed
        with exit 0 and the hook never ran."""
        for command in ("git commit --no-verif -m ok", "git commit --no-veri -m ok",
                        "git commit --no-verify=true -m ok",
                        "git push --no-verif origin main"):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_an_ambiguous_abbreviation_is_not_claimed_to_be_the_flag(self):
        """`--no-ver` is ambiguous between `--no-verbose` and `--no-verify`, and
        git itself rejects it with exit 129 — it is not a bypass, so refusing it
        would be a false positive.  `--no-verbose` is not a prefix of
        `--no-verify`, so the prefix rule does not collide with it.

        NOTE: `--no-ver` is still REFUSED, because it starts with `--no-v`.
        That is over-matching on a command git would reject anyway, which is the
        cheap direction; it is asserted here so the behaviour is a decision on
        the record rather than an accident.
        """
        self.assertDenied("git commit --no-ver -m ok")
        self.assertAllowed("git commit --no-verbose -m ok")
        self.assertAllowed("git log --no-verbose")

    def test_a_subshell_no_longer_shifts_git_out_of_argv0(self):
        """`(` and `)` are emitted as tokens by `punctuation_chars` but were
        absent from SEPARATORS, so they were appended INTO the segment and a
        subshell reliably made `git` argv[N>0].  Confirmed against real git:
        `( git commit --no-verify -m ok )` committed, hook never ran."""
        for command in (
            "( git commit --no-verify -m ok )",
            "(git commit --no-verify -m ok)",
            "cd /repo && (git commit -n -m ok)",
            "echo $(git commit --no-verify -m ok)",
            "case x in x) git commit --no-verify -m y ;; esac",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_hooksPath_override_is_refused(self):
        """`git -c core.hooksPath=/dev/null commit -m ok` skips every hook while
        carrying neither `--no-verify` nor `-n`.  It is worse than the flag
        forms: it disables `post-commit` too, so the ledger that is supposed to
        RECORD an ungated commit never runs either.  Confirmed against real git:
        exit 0, hook never ran."""
        for command in (
            "git -c core.hooksPath=/dev/null commit -m ok",
            "git -c core.hooksPath=/tmp/empty commit -m ok",
            "git -c core.hooksPath=/dev/null push origin main",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_an_untokenizable_bypass_that_dodges_the_old_markers_is_refused(self):
        """`looks_like_bypass` used to test for the literal `--no-verify` or a
        bare `-n`.  `-nm` does not match `(?<!\\w)-n(?!\\w)` (the `m` is a word
        character) and `--no-verif` is not the substring `--no-verify`, so an
        untokenizable command using either dodged the fail-closed branch
        entirely.  The screen is wider now: `--no-v`, `core.hookspath=`, or any
        short cluster containing `n` near a `git`."""
        for command in (
            "git commit -nm 'unbalanced",
            'git commit --no-verif -m "unbalanced',
            'git commit -anm "unbalanced',
            "git -c core.hooksPath=/dev/null commit -m 'unbalanced",
        ):
            with self.subTest(command=command):
                proc = self.bash(command)
                self.assertEqual(proc.returncode, 2, proc.stderr)
                self.assertIn("could not be parsed", proc.stderr)

    def test_git_push_dash_n_is_a_dry_run_and_is_allowed(self):
        """`git push -h` says `-n, --dry-run`.  The hook refused it, and an
        older test asserted that refusal as if it were the spec.  Both are
        corrected; the evidence lives in
        `test_the_short_flag_is_only_a_bypass_where_git_honours_it`."""
        self.assertAllowed("git push -n origin main")
        self.assertAllowed("git push -n")
        self.assertAllowed("git merge -n feat")


# ---------------------------------------------------------------------------
# KNOWN DEFECTS still open after 5453bc4.  Same contract as the other
# KnownDefects classes: each asserts the CURRENT, WRONG behaviour, is green by
# construction, and must be INVERTED when its defect is fixed.  Nothing here is
# an endorsement.
#
# Every "-> real bypass" note was CONFIRMED against real git in a throwaway repo
# whose pre-commit hook prints and exits 1: the commit landed, the hook never
# printed.
# ---------------------------------------------------------------------------
class FixesLandedIn2cf0caea(DenyNoVerify):
    """Inversions for the third round.  Each was a test_DEFECT_* until 2cf0caea
    closed it; each was observed RED against `2cf0caea~1` and green on HEAD.
    """

    SCRIPT = Path(os.environ.get("DENY_HOOK_UNDER_TEST",
                                 str(_SCRIPTS_DIR / "deny-no-verify.py")))

    def test_a_backslash_line_continuation_no_longer_hides_the_flag(self):
        """A backslash-newline is a LINE CONTINUATION — the shell deletes it, so
        `git commit \\<nl>--no-verify -m ok` IS the bypass.  posix shlex treats
        the backslash as an ESCAPE instead, so the newline survived glued to the
        front of the next token (`'\\n--no-verify'`, `'\\ncommit'`), matching
        neither the flag nor the subcommand.

        This hole was present in EVERY version of the file including the first;
        it was not a regression.  `current.append(tok.lstrip("\\r\\n"))` reaches
        the shell's answer one step later.
        """
        for command in (
            "git commit \\\n--no-verify -m ok",
            "git \\\ncommit --no-verify -m ok",
            "git commit \\\n-nm ok",
            "git \\\n-c core.hooksPath=/dev/null commit -m ok",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_prose_mentioning_hooksPath_is_allowed_again(self):
        """The hooksPath screen was a substring test over EVERY token, so a
        commit message that merely discussed the setting was refused.  Only the
        VALUE of `-c` / `--config-env` is inspected now."""
        for command in (
            "git commit -m 'set core.hooksPath=.githooks to enable'",
            "git commit -m 'core.hooksPath=x'",
            "echo core.hooksPath=/dev/null",
        ):
            with self.subTest(command=command):
                self.assertAllowed(command)

    def test_a_hooksPath_value_is_still_refused_even_when_benign(self):
        """The deliberate over-match, asserted so it stays a decision on the
        record rather than becoming a surprise.

        `git -c core.hooksPath=.githooks commit` points at the repository's REAL
        hooks and would strengthen the gate, not skip it — and it is refused
        anyway.  The code's reasoning: telling that apart from `/dev/null` means
        resolving the path against the repo's own config, and guessing wrong in
        the permissive direction costs the gate, while the author can just omit
        an override that is already the default.

        I think that trade is right, and it is worth saying why rather than
        just recording it: the benign spelling has a zero-cost alternative
        (drop the flag), so the false positive is recoverable in one edit, while
        the permissive error is silent and permanent.  That asymmetry is the
        whole argument, and it does not depend on how likely either case is.
        """
        for command in (
            "git -c core.hooksPath=.githooks commit -m ok",
            "git -c core.hooksPath=/dev/null commit -m ok",
            "git --config-env=X -c core.hooksPath=/dev/null push",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)

    def test_an_option_value_is_not_read_as_a_flag(self):
        """`git commit -m -n` is a commit whose MESSAGE is the two characters
        `-n`; it was read as the short bypass flag.  Values of value-taking
        options are skipped now, and `--` ends the flag scan.

        Both halves were needed: the value-skip alone does not cover
        `git commit -m ok -- -n`, where `-n` is a pathspec rather than a value.
        Confirmed against real git that neither form is a bypass — both fail
        with `pathspec '-n' did not match any file(s) known to git`, so nothing
        is committed and no gate is skipped.
        """
        for command in (
            "git commit -m -n",
            "git commit -m ok -- -n",
            "git commit --author -n -m ok",
            "git commit -- --no-verify",
            "git commit -m ok -- --no-verify",
        ):
            with self.subTest(command=command):
                self.assertAllowed(command)

    def test_the_flag_before_a_double_dash_is_still_a_bypass(self):
        """Control for the `--` break: it must end the scan, not disable it.
        Without this, a hook that simply stopped scanning would pass the test
        above."""
        for command in (
            "git commit --no-verify -- file",
            "git commit -n -- file",
            "git commit -m ok --no-verify -- file",
        ):
            with self.subTest(command=command):
                self.assertDenied(command)


class KnownDefectsDenyNoVerify(DenyNoVerify):
    SCRIPT = Path(os.environ.get("DENY_HOOK_UNDER_TEST",
                                 str(_SCRIPTS_DIR / "deny-no-verify.py")))

    def test_DEFECT_REGRESSION_gpg_sign_swallows_the_bypass_flag(self):
        """DEFECT (severity: HIGH — a REGRESSION introduced by 2cf0caea, the
        commit that fixed the previous round's findings).

        `OPTS_WITH_VALUE` is skipped with a blanket `k += 2`, on the assumption
        that every option in it takes its value as a SEPARATE token.  Two do
        not.  From git's own help:

            -S, --gpg-sign[=<key-id>]

        The value is OPTIONAL and ATTACHED (`-S<keyid>`, `--gpg-sign=<keyid>`);
        git never consumes the following token.  The hook does, so whatever
        comes next is swallowed — including the bypass flag:

            git commit -S --no-verify -m ok   -> hook exit 0
            git commit -S -n -m ok            -> hook exit 0
            git commit --gpg-sign -n -m ok    -> hook exit 0

        Confirmed against real git with a stand-in `gpg.program`: all three
        COMMITTED with exit 0 and the pre-commit hook never ran.  Bisected
        across all four versions of this file — refused by the original, by
        365941f8 and by 5453b4bc; allowed only from 2cf0caea.

        Suggested fix: `-S`/`--gpg-sign` must not be in OPTS_WITH_VALUE, since
        an attached value needs no skip.  The general rule is that only options
        whose value is a mandatory SEPARATE token may be skipped; an optional or
        attached value must not consume the next token.  Not implemented here —
        the verifier does not certify their own repair.
        """
        for command in (
            "git commit -S --no-verify -m ok",
            "git commit -S -n -m ok",
            "git commit --gpg-sign -n -m ok",
            "git commit -m ok -S -n",
        ):
            with self.subTest(command=command):
                proc = self.bash(command)
                self.assertEqual(
                    proc.returncode,
                    0,
                    "DEFECT PINNED: %r is a real bypass and is allowed. When "
                    "fixed this must become assertDenied." % command,
                )
        # The boundary: an option whose value really IS a separate token must
        # still be skipped, so a fix cannot be claimed by deleting the skip.
        self.assertDenied("git commit -C HEAD -n -m ok")
        self.assertAllowed("git commit -m -n")

    def test_DEFECT_a_newline_only_argument_splits_the_segment(self):
        """DEFECT (severity: LOW as measured — a mis-parse whose exploitability
        I could NOT establish).

        NEWLINE_TOKEN matches any token that is entirely `\\r`/`\\n`, and it is
        tested before the token is appended — so a QUOTED argument that is just
        a newline is treated as a command separator:

            git commit -m "<newline>" --no-verify
              -> [['git', 'commit', '-m'], ['--no-verify']]

        The flag lands in a segment whose argv[0] is `--no-verify`, not `git`,
        and the hook exits 0.  The same shape has been present since 5453b4bc
        (which introduced NEWLINE_TOKEN); it is not from 2cf0caea.

        WHAT I COULD NOT SHOW: that this is reachable as an actual bypass.  The
        only vehicle I found is a commit message that is a lone newline, and
        real git REFUSES that — `exit 1, commits=0`, "empty commit message" —
        so nothing is committed and no gate is skipped.  I did not find another
        argument position where a newline-only token is both accepted by git and
        placed before the flag.

        It is recorded as a parse defect with UNVERIFIED exploitability rather
        than as a bypass, and it is NOT claimed to be safe: a vehicle I did not
        find is not a vehicle that does not exist.
        """
        for command in ('git commit -m "\n" --no-verify', 'git commit -m "\n" -n'):
            with self.subTest(command=command):
                proc = self.bash(command)
                self.assertEqual(
                    proc.returncode, 0, "DEFECT PINNED (mis-parse): %r" % command
                )
        # A newline token that is genuinely a separator must keep splitting.
        self.assertDenied("cargo test\ngit commit --no-verify -m x")


    def test_DEFECT_deny_hook_misses_an_interpreter_wrapper_or_prefix(self):
        """DEFECT (severity: HIGH) — named in the module docstring as open.

        `is_bypass` inspects argv[0] of each segment.  An interpreter takes the
        command as a single quoted ARGUMENT; a prefix command puts its own name
        in argv[0].  Either way `git` is argv[N>0] and the segment is dismissed.
        All confirmed reaching real git.

        The subshell family `( … )` / `$( … )` / `case … )` was the half of this
        that 5453bc4 DID close; it is asserted green in
        FixesLandedIn5453bc4.test_a_subshell_no_longer_shifts_git_out_of_argv0
        and deliberately not repeated here, so the fixed half cannot make this
        list look shorter than it is.
        """
        for command in (
            "sh -c 'git commit --no-verify -m ok'",
            'bash -lc "git commit --no-verify -m ok"',
            "eval 'git commit --no-verify -m ok'",
            "env git commit --no-verify -m ok",
            "nohup git commit --no-verify -m ok",
            "time git commit --no-verify -m ok",
            "echo ok | xargs -I{} git commit --no-verify -m {}",
            "if true; then git commit --no-verify -m ok; fi",
            "for i in 1; do git commit --no-verify -m ok; done",
        ):
            with self.subTest(command=command):
                proc = self.bash(command)
                self.assertEqual(
                    proc.returncode,
                    0,
                    "DEFECT PINNED: %r. When fixed this must become "
                    "assertDenied." % command,
                )



    def test_DEFECT_a_heredoc_body_quoting_the_command_is_refused(self):
        """DEFECT (severity: MEDIUM — a FALSE POSITIVE, and the one that made
        writing the commit message for this very fix impossible).

        A heredoc body is DATA, but shlex has no shell grammar and tokenizes it
        like argv.  Now that newlines split segments, a body LINE that begins
        with the command is a segment whose argv[0] is `git`:

            git commit -F - <<'EOF'
            git commit --no-verify was allowed
            EOF
                -> segment ['git','commit','--no-verify','was','allowed'] -> exit 2

        Prose that merely MENTIONS the flag is fine — only prose that quotes the
        command at the start of a line trips it.  The workaround is
        `git commit -F <file>`, which keeps the text out of the Bash string.

        Is it fixable at this layer?  PARTLY, and the argument is worth stating
        because it is not obvious.  A heredoc body is only executable when its
        consumer is an interpreter (`sh <<EOF`), and interpreter wrappers are
        ALREADY an open hole pinned above.  So dropping heredoc bodies — tokens
        between `<<DELIM` and `DELIM` — loses no coverage this hook currently
        has, and removes this false positive.  What is NOT fixable here is the
        general case: `$(…)` bodies genuinely are executable, so prose and code
        are indistinguishable without a real shell parser.  Documented rather
        than papered over, per the request that produced this test.
        """
        for command in (
            "git commit -F - <<'EOF'\nfix\n\ngit commit --no-verify was allowed\nEOF",
            "git commit -F - <<'EOF'\nObserved:\n  git commit -nm ok\nEOF",
        ):
            with self.subTest(command=command):
                proc = self.bash(command)
                self.assertEqual(
                    proc.returncode,
                    2,
                    "DEFECT PINNED (false positive): %r. When fixed this must "
                    "become assertAllowed." % command,
                )
        # Prose that does not START a line with the command is already fine —
        # the boundary, so a fix cannot be claimed by widening this test.
        self.assertAllowed(
            "git commit -F - <<'EOF'\nwe stopped using --no-verify\nEOF"
        )

    def test_DEFECT_looks_like_bypass_is_noisy_on_untokenizable_input(self):
        """DEFECT (severity: LOW — accepted over-matching, measured not guessed).

        The fail-closed screen fires when `"git" in command.lower()` and a
        `-[A-Za-z]*n[A-Za-z]*` cluster is present.  `"git"` is a SUBSTRING test,
        so it matches `digits`, `legitimate`, `github`, `.gitignore`; the
        cluster regex matches `-n` but also `-name`, `-ln`, `-not`, `-newer`.
        Any untokenizable command combining the two is refused:

            echo 'legitimate -name thing        -> exit 2
            ls -ln /Users/x/src/harness/.git |… -> exit 2
            cargo test --no-vendor 'unbalanced  -> exit 2   (via the --no-v arm)

        This only fires on input that does not lex, and the remedy is to balance
        the quote, so it is recorded as accepted cost rather than as a bug to
        fix.  It is pinned because "accepted" should be a decision someone made,
        not something discovered later by whoever hits it.
        """
        for command in (
            "echo 'legitimate -name thing",
            "echo 'github actions -n dry",
            "ls -ln /Users/x/src/harness/.git | grep 'foo",
            "cargo test --no-vendor 'unbalanced",
            "git log --no-verbose 'unbalanced",
        ):
            with self.subTest(command=command):
                proc = self.bash(command)
                self.assertEqual(
                    proc.returncode, 2, "DEFECT PINNED (over-match): %r" % command
                )
        # The screen is not unconditional: no marker, no refusal.
        self.assertAllowed("echo 'unbalanced")


if __name__ == "__main__":
    unittest.main(verbosity=2)
