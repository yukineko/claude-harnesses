#!/usr/bin/env python3
"""Repro tests for two bypass-ledger false positives (backlog 6267bfbe, 36c936bc).

Both run the REAL `.githooks/post-commit` and `.githooks/post-merge` from this
checkout against a throwaway repo. pre-commit / pre-merge-commit are replaced by a
stub that writes the gate certificate in exactly the format and from exactly the
inputs the real pre-commit uses (`.githooks/pre-commit`: `tree="$(git write-tree)"`,
`head="$(git rev-parse HEAD)"`, `printf '%s %s\\n' "$tree" "$head" >
"$GIT_DIR_LOCAL/gate-verified-tree"`), i.e. "every gate ran and went green".
The stub is the honest stand-in for a green gate; what is under test is only how
post-commit / post-merge read that certificate afterwards.

- 6267bfbe: `git commit --amend` of a gated commit, itself gated, is written to the
  bypass ledger as an UNGATED COMMIT (the certificate's HEAD is the commit being
  replaced, which is not a parent of the replacement).
- 36c936bc: `git merge --ff-only` onto a merge commit that was already gated in
  another worktree is written to the ledger: post-merge uses the parent count of
  HEAD as a proxy for "this operation created a merge commit", and an ff whose
  new HEAD happens to be a merge commit passes that proxy.

Both are open defects (false positives that fail closed); each test is marked
expectedFailure until fixed.

scripts/test_gate_bypass.py AmendIsNotABypass pins 6267bfbe too, but at HEAD
f764bfeb it fails at its seed commit ("scripts/check-fail-open-diff.py is missing")
because its SCANNERS fixture lags the real hook list, so it cannot observe the
defect. This file does not depend on the scanner list.
"""

import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
HOOKS_SRC = REPO / ".githooks"

STUB_GATE = """#!/bin/sh
# Stand-in for a GREEN .githooks/pre-commit: certify (tree, HEAD) exactly as it does.
GIT_DIR_LOCAL="$(git rev-parse --git-dir 2>/dev/null)"
if [ -n "${GIT_DIR_LOCAL:-}" ] && tree="$(git write-tree 2>/dev/null)"; then
    head="$(git rev-parse HEAD 2>/dev/null)" || head=""
    printf '%s %s\\n' "$tree" "$head" > "$GIT_DIR_LOCAL/gate-verified-tree"
fi
exit 0
"""


class Scratch:
    def __init__(self, test):
        self.root = Path(tempfile.mkdtemp(prefix="bl-6267-36c9-"))
        test.addCleanup(shutil.rmtree, self.root, True)
        self.home = self.root / "home"
        self.home.mkdir()
        self.hooks = self.root / "hooks"
        self.hooks.mkdir()
        for name in ("post-commit", "post-merge"):
            shutil.copy2(HOOKS_SRC / name, self.hooks / name)
        for name in ("pre-commit", "pre-merge-commit"):
            (self.hooks / name).write_text(STUB_GATE)
        for p in self.hooks.iterdir():
            p.chmod(0o755)
        self.main = self.root / "main"
        self.main.mkdir()
        self.env = dict(os.environ)
        self.env.update(
            HOME=str(self.home),
            GIT_CONFIG_GLOBAL=os.devnull,
            GIT_CONFIG_NOSYSTEM="1",
            # keep post-merge's optional `specguard map sync` out of the probe
            PATH="/usr/bin:/bin:/usr/sbin:/sbin",
        )
        self.git(self.main, "init", "-q", "-b", "main")
        self.git(self.main, "config", "user.name", "t")
        self.git(self.main, "config", "user.email", "t@example.invalid")
        self.git(self.main, "config", "core.hooksPath", str(self.hooks))

    def git(self, cwd, *args, check=True):
        p = subprocess.run(
            ["git", *args], cwd=cwd, env=self.env, capture_output=True, text=True
        )
        if check and p.returncode != 0:
            raise AssertionError(
                "git %s failed (%d)\n%s%s" % (args, p.returncode, p.stdout, p.stderr)
            )
        return p

    def write(self, cwd, name, text):
        (Path(cwd) / name).write_text(text)

    def ledger(self):
        common = self.git(self.main, "rev-parse", "--git-common-dir").stdout.strip()
        path = (self.main / common) if not os.path.isabs(common) else Path(common)
        path = path / "gate-bypassed"
        if not path.exists():
            return []
        return [l for l in path.read_text().splitlines() if l.strip()]


class AmendOfGatedCommit(unittest.TestCase):
    """backlog 6267bfbe"""

    @unittest.expectedFailure
    def test_gated_amend_is_not_recorded_as_ungated(self):
        s = Scratch(self)
        s.write(s.main, "a.txt", "one\n")
        s.git(s.main, "add", "a.txt")
        s.git(s.main, "commit", "-q", "-m", "first")
        s.write(s.main, "b.txt", "x\n")
        s.git(s.main, "add", "b.txt")
        s.git(s.main, "commit", "-q", "-m", "second")
        self.assertEqual(s.ledger(), [], "precondition: plain gated commits are clean")

        s.write(s.main, "b.txt", "y\n")
        s.git(s.main, "add", "b.txt")
        p = s.git(s.main, "commit", "-q", "--amend", "--no-edit")
        self.assertEqual(
            s.ledger(),
            [],
            "the amend went through the (green) gate, so the ledger must not "
            "record it as ungated; post-commit stderr: %r" % p.stderr,
        )


class FastForwardOntoGatedMerge(unittest.TestCase):
    """backlog 36c936bc"""

    @unittest.expectedFailure
    def test_ff_onto_merge_gated_in_other_worktree_is_not_recorded(self):
        s = Scratch(self)
        s.write(s.main, "a.txt", "base\n")
        s.git(s.main, "add", "a.txt")
        s.git(s.main, "commit", "-q", "-m", "base")

        wt = s.root / "wt"
        s.git(s.main, "worktree", "add", "-q", "-b", "feat", str(wt))
        s.write(wt, "f.txt", "feature\n")
        s.git(wt, "add", "f.txt")
        s.git(wt, "commit", "-q", "-m", "feature work")

        s.write(s.main, "m.txt", "main moved\n")
        s.git(s.main, "add", "m.txt")
        s.git(s.main, "commit", "-q", "-m", "main moves")

        # The merge commit is created (and gated) in the worktree, per CLAUDE.md 8.
        s.git(wt, "merge", "--no-ff", "-q", "-m", "Merge main into feat", "main")
        self.assertEqual(
            len(s.git(wt, "rev-parse", "HEAD^@").stdout.split()),
            2,
            "precondition: the worktree HEAD is a real merge commit",
        )
        self.assertEqual(s.ledger(), [], "precondition: the gated merge is clean")

        # main integrates by fast-forward: no commit is created by this operation.
        before = s.git(s.main, "rev-parse", "HEAD").stdout.strip()
        p = s.git(s.main, "merge", "--ff-only", "feat")
        after = s.git(s.main, "rev-parse", "HEAD").stdout.strip()
        self.assertIn(
            before,
            s.git(s.main, "rev-parse", "HEAD^@").stdout.split(),
            "precondition: this was a fast-forward onto the gated merge commit",
        )
        self.assertEqual(after, s.git(wt, "rev-parse", "HEAD").stdout.strip())
        self.assertEqual(
            s.ledger(),
            [],
            "a fast-forward creates no commit, and the merge commit it moved onto "
            "was gated in the worktree; recording it is a false bypass. "
            "post-merge stderr: %r" % p.stderr,
        )


if __name__ == "__main__":
    unittest.main()
