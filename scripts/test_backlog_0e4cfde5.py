#!/usr/bin/env python3
"""backlog 0e4cfde5 (dups 346b68b2, 3031365b): the .githooks/pre-commit
main-tree-guard looks for condukt only at $CONDUKT_BIN, <repo>/target/release,
<repo>/target/debug and PATH. A fresh worktree has no target/, and condukt is
installed only in the plugin cache
(~/.claude/plugins/cache/yukineko/condukt/<ver>/bin/{condukt,condukt-darwin-arm64}),
so every worktree commit is refused with "no condukt binary found".

Claimed property: when no other candidate exists, the hook resolves the
installed plugin-cache condukt and runs `condukt guard main-tree` with it.

The hook is driven as a subprocess in a throwaway repo whose scanners are stubs
that exit 0 (the scanner list is read from the hook itself), HOME is a fixture
holding only the plugin cache, and PATH holds only basic tools.

RED repro:  python3 -m pytest --runxfail scripts/test_backlog_0e4cfde5.py
"""
from __future__ import annotations

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

_REPO = Path(__file__).resolve().parent.parent
_HOOK = _REPO / ".githooks" / "pre-commit"

_SCANNER_STUB = "#!/usr/bin/env python3\nimport sys\nsys.exit(0)\n"
_CONDUKT_STUB = """#!/bin/sh
echo "$0 $*" >> "{log}"
exit 0
"""


def scanners_in_hook() -> list[str]:
    names = re.findall(r"^run\s+(check-[\w.-]+\.py)\s", _HOOK.read_text(), re.M)
    if not names:
        raise AssertionError("no `run check-*.py` lines found in %s" % _HOOK)
    return names


class Fixture:
    def __init__(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="bl-0e4cfde5-")).resolve()
        self.repo = self.root / "repo"
        self.home = self.root / "home"
        self.home.mkdir()
        self.log = self.root / "condukt.log"
        subprocess.run(["git", "init", "-q", str(self.repo)], check=True, capture_output=True)
        scripts = self.repo / "scripts"
        scripts.mkdir()
        for name in scanners_in_hook():
            p = scripts / name
            p.write_text(_SCANNER_STUB)
            p.chmod(0o755)
        self.hook = self.repo / "pre-commit"
        shutil.copy2(_HOOK, self.hook)
        self.hook.chmod(0o755)
        self.bindir = self.root / "bin"
        self.bindir.mkdir()
        for tool in ("git", "cat", "sh", "env", "python3"):
            src = shutil.which(tool)
            if src is None:
                raise unittest.SkipTest("required tool not on PATH: " + tool)
            os.symlink(src, self.bindir / tool)
        self.env = {
            "PATH": str(self.bindir),
            "HOME": str(self.home),
            "LC_ALL": "C.UTF-8",
            "GIT_CONFIG_NOSYSTEM": "1",
        }

    def condukt_stub(self, path: Path) -> Path:
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_text(_CONDUKT_STUB.format(log=self.log))
        path.chmod(0o755)
        return path

    def install_plugin_cache(self) -> None:
        bindir = self.home / ".claude" / "plugins" / "cache" / "yukineko" / "condukt" / "0.7.240" / "bin"
        self.condukt_stub(bindir / "condukt")
        self.condukt_stub(bindir / "condukt-darwin-arm64")

    def run(self) -> subprocess.CompletedProcess:
        return subprocess.run([str(self.hook)], cwd=str(self.repo), env=self.env,
                              capture_output=True, text=True, timeout=120)

    def condukt_calls(self) -> list[str]:
        return self.log.read_text().splitlines() if self.log.exists() else []


class PluginCacheCondukt(unittest.TestCase):
    def fixture(self) -> Fixture:
        f = Fixture()
        self.addCleanup(shutil.rmtree, f.root, True)
        return f

    def test_control_condukt_bin_stub_lets_the_hook_pass(self) -> None:
        """Anti-vacuity: with a condukt the hook finds, this fixture commits clean,
        so a failure of the case below is about discovery and nothing else."""
        f = self.fixture()
        f.env["CONDUKT_BIN"] = str(f.condukt_stub(f.root / "condukt-bin"))
        proc = f.run()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertEqual(f.condukt_calls(), [f"{f.root / 'condukt-bin'} guard main-tree"])

    def test_control_no_condukt_anywhere_blocks(self) -> None:
        f = self.fixture()
        proc = f.run()
        self.assertNotEqual(proc.returncode, 0)
        self.assertIn("no condukt binary found", proc.stderr)

    def test_plugin_cache_condukt_is_used_when_nothing_else_exists(self) -> None:
        f = self.fixture()
        f.install_plugin_cache()
        proc = f.run()
        self.assertNotIn("no condukt binary found", proc.stderr)
        calls = f.condukt_calls()
        self.assertEqual(len(calls), 1, f"plugin-cache condukt was not run; stderr={proc.stderr!r}")
        self.assertTrue(calls[0].endswith(" guard main-tree"), calls)
        self.assertIn("/.claude/plugins/cache/yukineko/condukt/", calls[0])
        self.assertEqual(proc.returncode, 0, proc.stderr)


if __name__ == "__main__":
    unittest.main()
