#!/usr/bin/env python3
"""backlog fb7c3d83: rollout-plugins.sh resolve_overwatch_bin keeps the
`$REPO/target` fallback that a0525604 removed from rebuild-plugins.sh.

rebuild-plugins.sh treats "cargo metadata exited 0 but reported no
target_directory" as "cannot determine the build dir" and exits 1 (CLAUDE.md
section 3). rollout-plugins.sh's resolve_overwatch_bin, in the same situation,
falls back to `tdir="${tdir:-$REPO/target}"` and returns whatever overwatch
binary is lying under $REPO/target -- a binary cargo did not say this build
writes, i.e. possibly stale -- and that binary then drives the canary gate.

The same stale pick also happens when cargo DOES report a different build dir
that has no overwatch yet: the candidate list appends the $REPO/target paths
unconditionally.

Only the function is extracted (same technique as test_backlog_c51acbdc.py)
and run in a temp REPO with a stub `cargo` on PATH; the rollout itself is
never executed.

Repro:  python3 -m pytest --runxfail scripts/test_backlog_fb7c3d83.py
"""
import os
import re
import subprocess
import tempfile
import unittest
from pathlib import Path

SCRIPTS = Path(__file__).resolve().parent
ROLLOUT = SCRIPTS / "rollout-plugins.sh"


def extract_function(src, name):
    m = re.search(r"^%s\(\) \{\n.*?^\}\n" % re.escape(name), src, re.S | re.M)
    if not m:
        raise AssertionError(f"function {name} not found in rollout-plugins.sh")
    return m.group(0)


def resolve(tmp, metadata_json, built_in_reported_dir=False):
    """Run resolve_overwatch_bin with a stub cargo whose `metadata` prints
    metadata_json and exits 0. A stale overwatch always sits under
    $REPO/target/release."""
    fn = extract_function(ROLLOUT.read_text(), "resolve_overwatch_bin")
    repo = tmp / "repo"
    stale = repo / "target" / "release" / "overwatch"
    stale.parent.mkdir(parents=True)
    stale.write_text("#!/bin/sh\necho stale\n")
    stale.chmod(0o755)
    reported = tmp / "elsewhere"
    reported.mkdir()
    if built_in_reported_dir:
        fresh = reported / "release" / "overwatch"
        fresh.parent.mkdir(parents=True)
        fresh.write_text("#!/bin/sh\necho fresh\n")
        fresh.chmod(0o755)
    pathdir = tmp / "pathbin"
    pathdir.mkdir()
    cargo = pathdir / "cargo"
    payload = metadata_json.replace("@REPORTED@", str(reported))
    (tmp / "meta.json").write_text(payload)
    cargo.write_text(f'#!/bin/sh\ncat "{tmp / "meta.json"}"\nexit 0\n')
    cargo.chmod(0o755)
    harness = tmp / "h.sh"
    harness.write_text(f'set -euo pipefail\nREPO="{repo}"\n{fn}\nresolve_overwatch_bin\n')
    env = dict(os.environ)
    env.pop("OVERWATCH_BIN", None)
    # stub cargo + system dirs only: no real overwatch on PATH.
    env["PATH"] = f"{pathdir}:/usr/bin:/bin"
    r = subprocess.run(["bash", str(harness)], capture_output=True, text=True, env=env)
    return r, str(stale)


class ResolveOverwatchBinTargetFallback(unittest.TestCase):
    def test_control_reported_dir_with_binary_is_used(self):
        """Control: cargo reports a build dir holding overwatch -> that one."""
        with tempfile.TemporaryDirectory() as t:
            tmp = Path(t).resolve()
            r, stale = resolve(
                tmp, '{"target_directory":"@REPORTED@"}', built_in_reported_dir=True)
            self.assertEqual(r.returncode, 0, r.stderr)
            self.assertEqual(r.stdout.strip(), str(tmp / "elsewhere/release/overwatch"))

    def test_metadata_without_target_directory_does_not_guess_repo_target(self):
        """backlog fb7c3d83: fixed (RED observed before the fix). cargo metadata exits 0
        with no target_directory: the build dir is undetermined, so the
        $REPO/target binary must not be selected (rebuild-plugins.sh exits 1
        here)."""
        with tempfile.TemporaryDirectory() as t:
            r, stale = resolve(Path(t).resolve(), '{"packages":[]}')
            self.assertNotEqual(
                r.stdout.strip(), stale,
                "resolve_overwatch_bin guessed $REPO/target although cargo "
                f"metadata reported no target_directory (rc={r.returncode})")

    def test_reported_dir_without_binary_does_not_fall_back_to_repo_target(self):
        """backlog fb7c3d83: fixed (RED observed before the fix). cargo reports a
        different build dir with no overwatch built yet: the $REPO/target
        binary is not this build's and must not be selected."""
        with tempfile.TemporaryDirectory() as t:
            r, stale = resolve(Path(t).resolve(), '{"target_directory":"@REPORTED@"}')
            self.assertNotEqual(
                r.stdout.strip(), stale,
                "resolve_overwatch_bin selected the stale $REPO/target overwatch "
                f"although cargo reports a different build dir (rc={r.returncode})")


if __name__ == "__main__":
    unittest.main()
