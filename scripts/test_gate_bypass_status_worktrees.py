#!/usr/bin/env python3
"""gate-bypass.py status form vs other worktrees (user ruling 2026-11-08).

Split out of scripts/test_gate_bypass.py, which is at its file-growth cap;
reuses that file's GateHarness / GateTestCase.

    python3 scripts/test_gate_bypass_status_worktrees.py
"""

from __future__ import annotations

import json
import shutil
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_gate_bypass import GateTestCase, _which  # noqa: E402


# ---------------------------------------------------------------------------
# The status form (no --push) no longer looks at other worktrees (user ruling
# 2026-11-08: hooks check only the change's range; gate-bypass.py loses
# head_tips and every `git worktree list` call). An entry counts as reachable
# only when some ref (`for-each-ref --contains`) or THIS worktree's HEAD
# contains it. test_reachable_only_from_a_detached_HEAD_stays_outstanding above
# still pins the THIS-worktree half.
# ---------------------------------------------------------------------------
class StatusFormIgnoresOtherWorktrees(GateTestCase):
    def _out(self, proc):
        return "--- exit %d ---\n--- stdout ---\n%s\n--- stderr ---\n%s" % (
            proc.returncode, proc.stdout, proc.stderr)

    def _detached_bypass_held_by_other_worktree(self, h):
        """An ungated commit whose only holder is ANOTHER worktree's detached
        HEAD: made on a detached HEAD here, then this worktree goes back to
        main and a second worktree is checked out (detached) at it."""
        h.git("checkout", "-q", "--detach")
        h.write("a.txt", "detached\n")
        h.git("add", "-A")
        self.assertEqual(h.commit("other-wt-bypass", "--no-verify").returncode, 0)
        sha = h.git("rev-parse", "HEAD").stdout.strip()
        h.git("checkout", "-q", "main")
        other = h.root.parent / (h.root.name + "-other-wt")
        self.addCleanup(shutil.rmtree, str(other), True)
        h.git("worktree", "add", "-q", "--detach", str(other), sha)
        self.assertIn(sha, [ln.split("\t")[0] for ln in h.ledger_lines()],
                      "precondition: the bypass was recorded")
        self.assertEqual(h.git("for-each-ref", "--contains", sha).stdout.strip(), "",
                         "precondition: no ref contains it")
        self.assertNotEqual(
            h.git("merge-base", "--is-ancestor", sha, "HEAD", check=False).returncode, 0,
            "precondition: this worktree's HEAD does not contain it")
        self.assertEqual(h.git("rev-parse", "HEAD", cwd=other).stdout.strip(), sha,
                         "precondition: the other worktree's HEAD is the commit")
        return sha

    def test_status_form_runs_no_git_worktree_list(self):
        h = self.harness()
        self._detached_bypass_held_by_other_worktree(h)
        log = h.root.parent / (h.root.name + "-git-argv.log")
        self.addCleanup(lambda: log.unlink() if log.exists() else None)
        wrapper = h.root / ".stub-bin" / "git"
        wrapper.write_text(
            "#!/bin/sh\n"
            "printf '%%s\\n' \"$*\" >> '%s'\n"
            'exec "%s" "$@"\n' % (log, _which("git"))
        )
        wrapper.chmod(0o755)
        proc = h.gate_bypass()
        calls = log.read_text().splitlines() if log.exists() else []
        # anti-vacuity: the wrapper saw the reachability lookups.
        self.assertTrue(any("for-each-ref" in c for c in calls),
                        "the git wrapper did not see the status form's calls: %r\n%s"
                        % (calls, self._out(proc)))
        self.assertEqual([c for c in calls if "worktree" in c.split()], [],
                         "the status form must not consult other worktrees")

    def test_commit_held_only_by_another_worktrees_head_is_not_outstanding(self):
        h = self.harness()
        sha = self._detached_bypass_held_by_other_worktree(h)
        proc = h.gate_bypass()
        self.assertEqual(proc.returncode, 0, self._out(proc))
        self.assertIn(sha[:12], proc.stdout + proc.stderr,
                      "it stays listed (as unreachable), not dropped")
        payload = json.loads(h.gate_bypass("--json").stdout)
        self.assertEqual(payload["entries"], [], payload)
        self.assertEqual([e["commit"] for e in payload["unreachable"]], [sha], payload)

    def test_control_the_same_commit_reachable_from_a_ref_is_outstanding(self):
        h = self.harness()
        sha = self._detached_bypass_held_by_other_worktree(h)
        h.git("branch", "keep", sha)
        proc = h.gate_bypass()
        self.assertEqual(proc.returncode, 1, self._out(proc))
        self.assertIn(sha[:12], proc.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
