#!/usr/bin/env python3
"""Repro for backlog 73b167d3: when no per-platform binary is bundled, the tdd
launcher (crates/tdd/bin/tdd) tells the user to recover with "Build it with
scripts/build-plugin-bin.sh for <os>-<arch>". The launcher runs from the plugin
CACHE, which is a copy of crates/tdd alone (git-subdir); its Cargo.toml inherits
`rust-version.workspace = true` etc. from a workspace root that is not shipped,
so the advised build cannot work there (measured: cargo exits 101, "failed to
find a workspace root").

The test reproduces the cache layout (a lone copy of crates/tdd, nothing above
it), confirms the launcher really advises that script, then FOLLOWS the advice
and requires it to produce the binary the launcher looks for.
Slow only after a fix (a real release build); today it fails in < 1 s.
"""
import json
import os
import platform
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CRATE = REPO / "crates" / "tdd"


def host() -> str:
    os_ = {"Darwin": "darwin", "Linux": "linux"}.get(platform.system(), platform.system())
    arch = {"x86_64": "x86_64", "amd64": "x86_64", "arm64": "arm64", "aarch64": "arm64"}.get(
        platform.machine(), platform.machine())
    return f"{os_}-{arch}"


def cache_copy(dst: Path) -> Path:
    plug = dst / "tdd" / "0.0.0"
    shutil.copytree(CRATE, plug, ignore=shutil.ignore_patterns("tdd-*-*", "target"))
    return plug


class CacheRecoveryAdvice(unittest.TestCase):
    def test_control_launcher_in_cache_advises_the_build_script(self):
        with tempfile.TemporaryDirectory() as t:
            plug = cache_copy(Path(t))
            r = subprocess.run(["sh", str(plug / "bin" / "tdd"), "gate"],
                               input=json.dumps({"stop_hook_active": False}),
                               capture_output=True, text=True)
            self.assertIn('"decision":"block"', r.stdout)
            self.assertIn("scripts/build-plugin-bin.sh", r.stdout)

    @unittest.expectedFailure  # backlog 73b167d3: open defect, remove when fixed
    def test_following_the_advice_inside_the_cache_builds_the_binary(self):
        with tempfile.TemporaryDirectory() as t:
            plug = cache_copy(Path(t))
            script = plug / "scripts" / "build-plugin-bin.sh"
            self.assertTrue(script.is_file(), "advised script is not even shipped in the cache")
            env = dict(os.environ, CARGO_TARGET_DIR=str(Path(t) / "target"))
            r = subprocess.run(["bash", str(script)], cwd=plug, env=env,
                               capture_output=True, text=True)
            built = plug / "bin" / f"tdd-{host()}"
            # The script swallows cargo's stderr (2>/dev/null under pipefail),
            # so surface the underlying cause next to its exit code.
            why = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1"],
                                 cwd=plug, env=env, capture_output=True, text=True)
            self.assertTrue(r.returncode == 0 and built.is_file(),
                            f"advised recovery fails inside the cache (exit {r.returncode}):\n"
                            f"{(r.stdout + r.stderr)[-1500:]}\n"
                            f"cargo metadata in the cache copy: exit {why.returncode}\n"
                            f"{why.stderr[-800:]}")


if __name__ == "__main__":
    unittest.main()
