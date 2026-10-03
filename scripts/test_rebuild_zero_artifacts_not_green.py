#!/usr/bin/env python3
"""IMPLEMENTER-WRITTEN tests for backlog a0525604 (written by the implementer of
the fix, NOT by an independent RED author — weigh them accordingly; the
independent pins are BacklogA0525604* in scripts/test_backlog_audit_b1_0.py).

Defect class under test: a REAL (non --dry-run) rebuild-plugins.sh run in which
the build produced NOT ONE of the release artifacts the cache refresh needs.
Every in-scope current-version plugin then takes the main loop's `missing`
branch, nothing is deployed, and the run used to exit 0 behind a stderr
WARNING — "could not deploy anything" reading as a green run (CLAUDE.md §3).
That is the state a wrong build dir (the empty-metadata half of a0525604)
produced; it must fail on its own, not only via the metadata guard.

Not over-blocking: a PARTIAL miss (some artifacts present) stays a warning — the
loop's documented "non-workspace or renamed bin" case — and --dry-run, which
builds nothing, stays a preview that exits 0.

Hermetic: a COPY of the script runs inside a throwaway git repo with fake
crates, a temp CLAUDE_PLUGIN_CACHE, a fake target dir and a `cargo` shim. The
real cache and the real crates/ are never read or written.

Run: python3 scripts/test_rebuild_zero_artifacts_not_green.py
"""
import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
REBUILD = Path(os.environ.get("REBUILD_SCRIPT", REPO / "scripts" / "rebuild-plugins.sh"))

PLUGINS = {"alpha": "1.0.0", "beta": "2.0.0"}


def host_suffix() -> str:
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    triple = re.search(r"^host: (.*)$", out, re.M).group(1)
    os_ = "darwin" if "apple-darwin" in triple else "linux" if "linux" in triple else "unknown"
    arch = "x86_64" if triple.startswith("x86_64-") else "arm64" if triple.startswith("aarch64-") else "unknown"
    return f"{os_}-{arch}"


class Fixture:
    def __init__(self, built):
        self.tmp = Path(tempfile.mkdtemp(prefix="rebuild-zeroart."))
        self.suf = host_suffix()
        self.repo = self.tmp / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        shutil.copy2(REBUILD, self.repo / "scripts" / "rebuild-plugins.sh")
        shutil.copy2(REPO / "scripts" / "cap-target-dir.sh", self.repo / "scripts" / "cap-target-dir.sh")
        core = self.repo / "crates" / "harness-core"
        core.mkdir(parents=True)
        (core / "Cargo.toml").write_text('[package]\nname = "harness-core"\nversion = "9.9.9"\n')
        for name, ver in PLUGINS.items():
            cp = self.repo / "crates" / name / ".claude-plugin"
            cp.mkdir(parents=True)
            (cp / "plugin.json").write_text(json.dumps({"name": name, "version": ver}) + "\n")
        env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t",
                   GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t")
        for cmd in (["git", "init", "-q"], ["git", "add", "."],
                    ["git", "-c", "commit.gpgsign=false", "commit", "-q", "--no-verify", "-m", "fixture"]):
            subprocess.run(cmd, cwd=self.repo, env=env, check=True, capture_output=True)

        # The build dir cargo metadata reports. `built` = which artifacts exist.
        self.target = self.tmp / "target"
        (self.target / "release").mkdir(parents=True)
        for name in built:
            f = self.target / "release" / name
            f.write_bytes(f"NEW-{name}\n".encode())
            f.chmod(0o755)

        shim = self.tmp / "shim"
        shim.mkdir()
        cargo = shim / "cargo"
        cargo.write_text(
            "#!/bin/sh\n"
            'if [ "$1" = metadata ]; then printf \'{"target_directory":"%s"}\\n\' "$FAKE_TARGET"; fi\n'
            "exit 0\n"
        )
        cargo.chmod(0o755)

        self.cache = self.tmp / "cache"
        for name, ver in PLUGINS.items():
            vd = self.cache / name / ver
            (vd / "bin").mkdir(parents=True)
            (vd / "bin" / name).write_text("#!/bin/sh\n")  # launcher
            b = vd / "bin" / f"{name}-{self.suf}"
            b.write_bytes(f"OLD-{name}\n".encode())
            b.chmod(0o755)
        self.env = dict(os.environ, PATH=f"{shim}:{os.environ['PATH']}", FAKE_TARGET=str(self.target),
                        CARGO_TARGET_CAP_MB="0", CLAUDE_PLUGIN_CACHE=str(self.cache))

    def hostbin(self, name):
        return (self.cache / name / PLUGINS[name] / "bin" / f"{name}-{self.suf}").read_bytes()

    def run(self, *args):
        return subprocess.run(["bash", str(self.repo / "scripts" / "rebuild-plugins.sh"), "--no-clean", *args],
                              cwd=self.repo, env=self.env, capture_output=True, text=True, timeout=120)

    def cleanup(self):
        shutil.rmtree(self.tmp, ignore_errors=True)


class ZeroArtifactsIsNotGreen(unittest.TestCase):
    def fx(self, built):
        f = Fixture(built)
        self.addCleanup(f.cleanup)
        return f

    def test_real_run_with_no_artifact_for_any_plugin_exits_nonzero(self):
        fx = self.fx(built=[])
        p = fx.run()
        self.assertNotEqual(p.returncode, 0,
                            "a0525604: a real run that deployed NOTHING (every plugin 'missing') "
                            f"exited 0:\n{p.stdout}{p.stderr}")
        self.assertIn("ERROR", p.stderr)
        self.assertIn(str(fx.target / "release"), p.stderr, "the error must name the build dir it looked in")
        self.assertEqual(fx.hostbin("alpha"), b"OLD-alpha\n")

    def test_only_scoped_run_with_no_artifact_in_scope_exits_nonzero(self):
        # beta's artifact exists but is outside --only; alpha's does not.
        fx = self.fx(built=["beta"])
        p = fx.run("--only=alpha")
        self.assertNotEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(fx.hostbin("beta"), b"OLD-beta\n", "--only=alpha touched beta")

    def test_partial_miss_stays_a_warning(self):
        fx = self.fx(built=["alpha"])
        p = fx.run()
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("WARNING: no release artifact for: beta", p.stderr)
        self.assertEqual(fx.hostbin("alpha"), b"NEW-alpha\n")

    def test_all_artifacts_present_is_green(self):
        fx = self.fx(built=["alpha", "beta"])
        p = fx.run()
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(fx.hostbin("alpha"), b"NEW-alpha\n")
        self.assertEqual(fx.hostbin("beta"), b"NEW-beta\n")

    def test_dry_run_with_no_artifacts_is_a_preview_not_a_failure(self):
        # --dry-run builds nothing, so absent artifacts say nothing about the
        # real run; it must still warn, not silently pass.
        fx = self.fx(built=[])
        p = fx.run("--dry-run")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertIn("WARNING: no release artifact for:", p.stderr)


if __name__ == "__main__":
    unittest.main(verbosity=2)
