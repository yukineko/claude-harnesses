#!/usr/bin/env python3
"""RED repro: condukt's tests need a `tdd` binary on ambient PATH, so the donegate
Stop hook (Claude Code app env, no plugin-cache bin dirs on PATH) fails them with
`DEPENDENCY MISSING: no `tdd` binary on PATH` although this workspace builds the
`tdd` crate (crates/tdd).

Reading (a) DEFECT: test/script should obtain tdd from the workspace build
  (cargo build -p tdd -> target/<profile>/tdd) instead of ambient PATH. Fix lives in
  crates/condukt/tests/*.rs `find_tdd` or scripts/test-changed-crates.sh.
Reading (b) INTENDED: the hook env must supply PATH; fix lives in donegate/hook env.
Telling them apart: this test already removes plugin-cache dirs from PATH while
keeping cargo, i.e. it reproduces the hook env. If the workspace has crates/tdd
(asserted in the control) the binary was obtainable without PATH -> (a) is
available and cheapest; (b) is the right answer only if a maintainer rules that
tests may depend on installed plugins. Only a maintainer ruling decides; this
test asserts (a) because it is the only reading checkable from inside the repo.

    python3 scripts/test_condukt_tdd_dependency_hook_path_20261107.py
"""
from __future__ import annotations

import os
import shutil
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
TEST = "backlog_records_lost_at_teardown_20261107"
CONTROL = "control_check_oracle_valid_before_removal"


def _hook_like_path() -> str:
    cargo = shutil.which("cargo")
    dirs = []
    if cargo:
        dirs.append(str(Path(cargo).parent))
    dirs += ["/usr/bin", "/bin", "/usr/sbin", "/sbin"]
    return ":".join(dirs)


def _has_tdd(path: str) -> bool:
    return any((Path(d) / "tdd").is_file() for d in path.split(":"))


class CondukTddDependencyUnderHookPath(unittest.TestCase):
    def test_control_workspace_builds_tdd_and_path_is_hook_like(self):
        self.assertTrue((REPO / "crates/tdd/Cargo.toml").is_file(),
                        "control: workspace must contain crates/tdd")
        self.assertIsNotNone(shutil.which("cargo"), "control: cargo needed")
        self.assertFalse(_has_tdd(_hook_like_path()),
                         "control: simulated hook PATH must not already hold tdd")

    def test_condukt_control_test_passes_without_ambient_tdd(self):
        env = dict(os.environ)
        env["PATH"] = _hook_like_path()
        env.setdefault("HOME", str(Path.home()))
        r = subprocess.run(
            ["cargo", "test", "-p", "condukt", "--test", TEST, "--",
             "--exact", CONTROL],
            cwd=REPO, env=env, capture_output=True, text=True)
        out = r.stdout + r.stderr
        self.assertEqual(
            r.returncode, 0,
            "condukt test fails under a hook-like PATH (cargo present, no plugin "
            "cache dirs):\n" + "\n".join(
                l for l in out.splitlines() if "DEPENDENCY MISSING" in l or "panicked" in l))


if __name__ == "__main__":
    unittest.main()
