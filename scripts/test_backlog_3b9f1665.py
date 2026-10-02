#!/usr/bin/env python3
"""Repro test for backlog 3b9f1665: the two plugin-binary build paths resolve
dependency features differently, so the repo bin and the cache bin can drift.

scripts/build-plugin-bin.sh builds one member (`cargo build --release -p <pkg>`),
scripts/rebuild-plugins.sh builds the workspace (`cargo build --release
--workspace --bins`). Cargo unifies features over the SELECTED packages, so a
dependency of <pkg> can be compiled with a different feature set by the two.

The test reads which selection each script's `cargo build` line uses, then asks
cargo (`cargo tree -e normal,build -f '{p} [{f}]'`) what features each
dependency of <pkg> gets under each selection, and requires them to be equal.
It goes green if both scripts converge on one selection (the item's proposal:
one update path) or if feature resolution stops differing. Open; expectedFailure.
"""

import re
import subprocess
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
PKG = "backlog"  # a plugin package the auditor measured drift for at e70d48dd


def _cargo():
    c = Path.home() / ".cargo" / "bin" / "cargo"
    return str(c) if c.exists() else "cargo"


def selection(script):
    """'workspace' or 'package', from the script's real cargo build line(s)."""
    lines = [
        l for l in (REPO / "scripts" / script).read_text().splitlines()
        if re.match(r"\s*cargo build\b", l)
    ]
    if not lines:
        raise AssertionError("no `cargo build` line in %s" % script)
    modes = {"workspace" if "--workspace" in l else "package" for l in lines}
    if len(modes) != 1:
        raise AssertionError("%s mixes selections: %r" % (script, lines))
    return modes.pop()


def features(mode):
    args = [_cargo(), "tree", "-e", "normal,build", "-f", "{p} [{f}]", "--prefix", "none"]
    args += ["--workspace"] if mode == "workspace" else ["-p", PKG]
    p = subprocess.run(args, cwd=REPO, capture_output=True, text=True, timeout=600)
    if p.returncode != 0:
        raise AssertionError("cargo tree failed: %s" % p.stderr[-2000:])
    out = {}
    for l in p.stdout.splitlines():
        m = re.match(r"(\S+ v\S+)(?: \(.*?\))? \[(.*?)\]", l)
        if m:
            out.setdefault(m.group(1), set()).update(f for f in m.group(2).split(",") if f)
    return out


def pkg_deps():
    p = subprocess.run(
        [_cargo(), "tree", "-e", "normal,build", "-p", PKG, "-f", "{p}", "--prefix", "none"],
        cwd=REPO, capture_output=True, text=True, timeout=600,
    )
    return {re.match(r"(\S+ v\S+)", l).group(1) for l in p.stdout.splitlines() if l.strip()}


class BuildPathsAgreeOnFeatures(unittest.TestCase):
    @unittest.expectedFailure
    def test_same_dependency_features_on_both_build_paths(self):
        a = selection("build-plugin-bin.sh")
        b = selection("rebuild-plugins.sh")
        fa, fb = features(a), features(b)
        diff = {
            dep: (sorted(fa.get(dep, set())), sorted(fb.get(dep, set())))
            for dep in sorted(pkg_deps())
            if fa.get(dep, set()) != fb.get(dep, set())
        }
        self.assertEqual(
            diff, {},
            "build-plugin-bin.sh (%s) vs rebuild-plugins.sh (%s) compile %s's "
            "dependencies with different features: %r" % (a, b, PKG, diff),
        )


if __name__ == "__main__":
    unittest.main()
