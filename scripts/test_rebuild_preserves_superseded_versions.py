#!/usr/bin/env python3
"""RED test for backlog 8acb117a: rebuild-plugins.sh must not swap a new binary
(or hooks config) into a SUPERSEDED plugin version dir.

Observed at HEAD (2026-10-01, real cache, read-only): for condukt / backlog /
overwatch every version dir holds a distinct inode for bin/<name>-darwin-arm64
but with identical size and identical mtime (the time of the last rollout), and
their .deployed-from.json name the newest commit. rebuild-plugins.sh's refresh
loop globs "$CACHE"/*/*/bin/*-$SUF, i.e. EVERY version dir, and `cp -f`s the
fresh build over each. A session pinned to an old version therefore executes
new code, and a canary rollback (re-point to the previous version dir) reverts
nothing.

The CURRENT version of a plugin is the one in crates/<c>/.claude-plugin/plugin.json.

Stdlib only. Never touches the real cache: every case runs against a temp
CLAUDE_PLUGIN_CACHE, a fake CARGO_TARGET_DIR and a `cargo` shim on PATH
(`metadata` prints the fake target dir, `build` is a no-op; rustc is real).

Run: python3 scripts/test_rebuild_preserves_superseded_versions.py
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
REBUILD = REPO / "scripts" / "rebuild-plugins.sh"
PLUGIN = "backlog"  # a real, non-gate plugin with a hooks/hooks.json


def host_suffix() -> str:
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    triple = re.search(r"^host: (.*)$", out, re.M).group(1)
    os_ = "darwin" if "apple-darwin" in triple else "linux" if "linux" in triple else "unknown"
    arch = "x86_64" if triple.startswith("x86_64-") else "arm64" if triple.startswith("aarch64-") else "unknown"
    return f"{os_}-{arch}"


def current_version() -> str:
    pj = json.loads((REPO / "crates" / PLUGIN / ".claude-plugin" / "plugin.json").read_text())
    return pj["version"]


class RebuildSuperseded(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="rebuild-superseded."))
        self.addCleanup(shutil.rmtree, self.tmp, ignore_errors=True)
        self.suf = host_suffix()
        self.cur = current_version()
        self.old = "0.0.1"
        self.assertNotEqual(self.cur, self.old)

        target = self.tmp / "target"
        (target / "release").mkdir(parents=True)
        new = target / "release" / PLUGIN
        new.write_bytes(b"NEW-BUILD\n")
        new.chmod(0o755)

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
        for ver in (self.old, self.cur):
            b = self.cache / PLUGIN / ver / "bin"
            b.mkdir(parents=True)
            f = b / f"{PLUGIN}-{self.suf}"
            f.write_bytes(f"OLD-{ver}\n".encode())
            f.chmod(0o755)
            h = self.cache / PLUGIN / ver / "hooks"
            h.mkdir()
            (h / "hooks.json").write_text(f'{{"stale-for":"{ver}"}}\n')

        env = dict(os.environ)
        env.update(
            PATH=f"{shim}:{env['PATH']}",
            FAKE_TARGET=str(target),
            CLAUDE_PLUGIN_CACHE=str(self.cache),
            CARGO_TARGET_CAP_MB="0",
        )
        self.proc = subprocess.run(
            ["bash", str(REBUILD), "--no-clean", f"--only={PLUGIN}"],
            cwd=REPO, env=env, capture_output=True, text=True,
        )

    def binp(self, ver):
        return self.cache / PLUGIN / ver / "bin" / f"{PLUGIN}-{self.suf}"

    def test_run_itself_succeeded(self):
        # precondition so the assertions below are not vacuous
        self.assertEqual(self.proc.returncode, 0, self.proc.stdout + self.proc.stderr)

    def test_control_current_version_binary_is_refreshed(self):
        # CONTROL: must stay green. A fix that stops refreshing everything fails here.
        self.assertEqual(self.binp(self.cur).read_bytes(), b"NEW-BUILD\n", self.proc.stdout)

    def test_superseded_version_binary_is_not_replaced(self):
        got = self.binp(self.old).read_bytes()
        self.assertEqual(
            got, f"OLD-{self.old}\n".encode(),
            f"superseded {PLUGIN}/{self.old} now runs the new build ({got!r}); "
            f"version pin / canary rollback is void.\n{self.proc.stdout}",
        )

    def test_superseded_version_hooks_config_is_not_replaced(self):
        got = (self.cache / PLUGIN / self.old / "hooks" / "hooks.json").read_text()
        self.assertEqual(
            got, f'{{"stale-for":"{self.old}"}}\n',
            f"superseded {PLUGIN}/{self.old} hooks.json was overwritten from the repo",
        )

    def test_control_current_version_hooks_config_is_refreshed(self):
        got = (self.cache / PLUGIN / self.cur / "hooks" / "hooks.json").read_text()
        self.assertEqual(got, (REPO / "crates" / PLUGIN / "hooks" / "hooks.json").read_text())


if __name__ == "__main__":
    unittest.main()
