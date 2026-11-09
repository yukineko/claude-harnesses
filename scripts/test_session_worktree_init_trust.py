#!/usr/bin/env python3
"""session-worktree-init.py must register the worktree it creates in the
workspace-trust list (~/.harness/trust.toml) iff the main repo is itself LISTED.

USER RULING 2026-11-08: worktree trust inheritance was removed from
harness_core::trust, so a linked worktree is trusted only when it is itself
listed; the creator registers it at creation. Membership only: an UNLISTED
main tree (even under HARNESS_TRUST_ALL) must not get its worktree listed
(propagating trust is not granting it). An unparseable trust list is never
rewritten (it would drop every entry).

Hermetic: HOME is a temp dir, HARNESS_SANDBOX_HOME / HARNESS_TRUST_ALL are
removed from the child env; the real ~/.harness/trust.toml is never touched.

Run: python3 scripts/test_session_worktree_init_trust.py
"""
import json
import os
import subprocess
import sys
import tempfile
import tomllib
import unittest
from pathlib import Path

HOOK = Path(__file__).resolve().parent / "session-worktree-init.py"


def git(cwd, *args):
    subprocess.run(("git", *args), cwd=cwd, check=True, capture_output=True)


class Fixture:
    def __init__(self):
        self.tmp = tempfile.TemporaryDirectory()
        base = Path(os.path.realpath(self.tmp.name))
        self.home = base / "home"
        self.home.mkdir()
        self.repo = base / "repo"
        self.repo.mkdir()
        git(self.repo, "init", "-q", "-b", "main")
        git(self.repo, "config", "user.email", "t@t.t")
        git(self.repo, "config", "user.name", "t")
        (self.repo / "a.txt").write_text("a\n")
        git(self.repo, "add", "a.txt")
        git(self.repo, "commit", "-q", "-m", "seed")

    @property
    def trust_file(self):
        return self.home / ".harness" / "trust.toml"

    def list_trusted(self, entries):
        self.trust_file.parent.mkdir(parents=True, exist_ok=True)
        self.trust_file.write_text(
            "trusted = [" + ", ".join(json.dumps(str(e)) for e in entries) + "]\n"
        )

    def trusted(self):
        if not self.trust_file.exists():
            return []
        return tomllib.loads(self.trust_file.read_text()).get("trusted", [])

    def run_hook(self, cwd, sid, **extra_env):
        env = dict(os.environ, HOME=str(self.home))
        env.pop("HARNESS_SANDBOX_HOME", None)
        env.pop("HARNESS_TRUST_ALL", None)
        env.update(extra_env)
        return subprocess.run(
            (sys.executable, str(HOOK)),
            input=json.dumps({"cwd": str(cwd), "session_id": sid}),
            capture_output=True, text=True, env=env, timeout=120,
        )

    def worktree_for(self, sid8):
        return self.repo.parent / ".repo-worktrees" / f"session-{sid8}"


class RegistersTrust(unittest.TestCase):
    def test_created_worktree_is_listed_when_main_is_listed(self):
        f = Fixture()
        f.list_trusted([f.repo])
        out = f.run_hook(f.repo, "abcd1234-0000")
        self.assertEqual(out.returncode, 0, out.stderr)
        wt = f.worktree_for("abcd1234")
        self.assertTrue(wt.is_dir(), f"worktree not created: {out.stdout}")
        trusted = f.trusted()
        self.assertIn(os.path.realpath(wt), trusted)
        self.assertIn(os.path.realpath(f.repo), trusted, "main entry must be preserved")
        self.assertNotIn("could NOT register", out.stdout)

    def test_unlisted_main_does_not_get_its_worktree_listed(self):
        f = Fixture()
        out = f.run_hook(f.repo, "abcd1234-0000")
        self.assertEqual(out.returncode, 0, out.stderr)
        wt = f.worktree_for("abcd1234")
        self.assertTrue(wt.is_dir(), f"apparatus: worktree created: {out.stdout}")
        self.assertNotIn(os.path.realpath(wt), f.trusted())
        self.assertNotIn("could NOT register", out.stdout,
                         "an unlisted main is nothing to propagate, not an error")

    def test_trust_all_env_does_not_mint_a_permanent_grant(self):
        # HARNESS_TRUST_ALL is transient; propagation is membership-only.
        f = Fixture()
        out = f.run_hook(f.repo, "abcd1234-0000", HARNESS_TRUST_ALL="1")
        self.assertEqual(out.returncode, 0, out.stderr)
        wt = f.worktree_for("abcd1234")
        self.assertTrue(wt.is_dir(), out.stdout)
        self.assertNotIn(os.path.realpath(wt), f.trusted())

    def test_other_entries_are_preserved_and_no_duplicates_on_rerun(self):
        f = Fixture()
        f.list_trusted([f.repo, "/some/other/project"])
        self.assertEqual(f.run_hook(f.repo, "abcd1234-0000").returncode, 0)
        wt = os.path.realpath(f.worktree_for("abcd1234"))
        self.assertEqual(f.run_hook(f.repo, "abcd1234-0000").returncode, 0)
        trusted = f.trusted()
        self.assertIn("/some/other/project", trusted)
        self.assertEqual(trusted.count(wt), 1, f"duplicate entries: {trusted}")

    def test_reused_worktree_created_before_the_rule_gets_listed(self):
        f = Fixture()
        # First run with the main UNLISTED: the worktree is created, not listed.
        self.assertEqual(f.run_hook(f.repo, "abcd1234-0000").returncode, 0)
        wt = os.path.realpath(f.worktree_for("abcd1234"))
        self.assertNotIn(wt, f.trusted())
        # Main gets listed later; the next session start reuses the worktree.
        f.list_trusted([f.repo])
        out = f.run_hook(f.repo, "abcd1234-0000")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertIn(wt, f.trusted())

    def test_unparseable_trust_list_is_never_rewritten_and_is_reported(self):
        f = Fixture()
        f.trust_file.parent.mkdir(parents=True)
        garbage = "trusted = [[[ not toml\n"
        f.trust_file.write_text(garbage)
        out = f.run_hook(f.repo, "abcd1234-0000")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertEqual(f.trust_file.read_text(), garbage,
                         "an unparseable list must never be rewritten")
        self.assertIn("could NOT register", out.stdout,
                      "the failed registration must be loud, not silent")

    def test_starting_inside_a_linked_worktree_lists_nothing(self):
        # The hook creates nothing from inside a linked worktree.
        f = Fixture()
        f.list_trusted([f.repo])
        wt = f.repo.parent / "linked"
        git(f.repo, "worktree", "add", "-q", "-b", "linked", str(wt))
        out = f.run_hook(wt, "ffff0000-1")
        self.assertEqual(out.returncode, 0, out.stderr)
        self.assertNotIn(os.path.realpath(wt), f.trusted())


if __name__ == "__main__":
    unittest.main()
