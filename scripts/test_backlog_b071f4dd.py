#!/usr/bin/env python3
"""Repro for backlog b071f4dd: condukt's unit test
diffrisk_record::tests::every_invocation_is_journaled_even_when_nothing_is_recorded
(and its sibling record_is_fail_soft_when_worktree_missing) call
`Config::load()`, whose state_dir is `$HOME/.condukt/state`. Only cwd and the
worktree are tempdirs, so every `cargo test -p condukt` appends diffrisk
ledgers into the user's LIVE ~/.condukt/state tree.

The test runs exactly that unit test with HOME pointed at an empty scratch dir
and requires the scratch HOME to stay untouched (a hermetic test writes only
into its own tempdirs). CARGO_HOME / RUSTUP_HOME are pinned to the real ones so
moving HOME does not move the toolchain.

Slow (builds condukt's bin unit tests): opt in with RUN_CARGO_REPROS=1.
"""
import os
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TEST = "diffrisk_record::tests::every_invocation_is_journaled_even_when_nothing_is_recorded"


@unittest.skipUnless(os.environ.get("RUN_CARGO_REPROS") == "1", "slow; set RUN_CARGO_REPROS=1")
class Hermeticity(unittest.TestCase):
    result = None

    @classmethod
    def setUpClass(cls):
        real = Path.home()
        cls.home = tempfile.TemporaryDirectory()
        env = {k: v for k, v in os.environ.items() if not k.startswith("CONDUKT")}
        env.update(HOME=cls.home.name,
                   CARGO_HOME=os.environ.get("CARGO_HOME", str(real / ".cargo")),
                   RUSTUP_HOME=os.environ.get("RUSTUP_HOME", str(real / ".rustup")))
        r = subprocess.run(["cargo", "test", "-p", "condukt", "--bin", "condukt", "--", "--exact", TEST],
                           cwd=REPO, env=env, capture_output=True, text=True)
        written = sorted(str(p.relative_to(cls.home.name))
                         for p in Path(cls.home.name).rglob("*") if p.is_file())
        cls.result = (r.returncode, r.stdout + r.stderr, written)

    @classmethod
    def tearDownClass(cls):
        cls.home.cleanup()

    def test_control_the_unit_test_actually_ran_and_passed(self):
        rc, out, _ = self.result
        self.assertEqual(rc, 0, out[-2000:])
        self.assertIn("1 passed", out)

    @unittest.expectedFailure  # backlog b071f4dd: open defect, remove when fixed
    def test_unit_test_writes_nothing_under_home(self):
        _, _, written = self.result
        self.assertEqual([w for w in written if w.startswith(".condukt")], [],
                         f"the test wrote into $HOME (live state in a normal run): {written}")


if __name__ == "__main__":
    unittest.main()
