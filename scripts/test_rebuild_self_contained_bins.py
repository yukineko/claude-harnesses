#!/usr/bin/env python3
"""Tests for the `crates/<crate>/self-contained-bins` declaration honoured by the
seed loop of scripts/rebuild-plugins.sh.

Written by the INDEPENDENT test author (not the implementer of the fix).

Bug: a plugin may ship a self-contained executable (e.g. flow-watch, a
`#!/usr/bin/env python3` script) next to its launcher. The seed loop treated
every non-platform-suffixed bin/<name> as a launcher needing
target/release/<name>, so `rollout-plugins.sh --plugin flow` died with
"could not seed a host binary into a FRESH version dir for: flow/0.2.40:flow-watch".

Contract tested (black-box: exit code / stdout / stderr / filesystem):
  * declared name: not seeded, no <name>-<suf> created, cache file byte-identical,
    stdout mentions `self-contained` + name; must be executable and non-empty.
  * declared but a workspace bin target, or metadata lacks "packages": error.
  * malformed declaration line: error naming the file.
  * NOT declared: unchanged (needs target/release/<name>, else the old ERROR).

Hermetic: a COPY of the script runs in a throwaway git repo with a cargo shim,
nm/file stubs, temp CLAUDE_PLUGIN_CACHE and sandboxed XDG_STATE_HOME. The real
cache, ~/.claude and the real crates/ are never touched (except the RealRepo
tests, which only READ the real repo).

Does NOT prove: that the real rollout-plugins.sh end-to-end deploys flow (only
rebuild-plugins.sh's seed loop is exercised); that real `cargo metadata` output
parses the same as the shim's; that flow-watch itself works; Windows suffixes.

Run: python3 scripts/test_rebuild_self_contained_bins.py -v
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
VER = "1.0.0"
WATCH_BODY = b"#!/usr/bin/env python3\nprint('watch')\n"


def host_suffix() -> str:
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    triple = re.search(r"^host: (.*)$", out, re.M).group(1)
    os_ = "darwin" if "apple-darwin" in triple else "linux" if "linux" in triple else "unknown"
    arch = "x86_64" if triple.startswith("x86_64-") else "arm64" if triple.startswith("aarch64-") else "unknown"
    return f"{os_}-{arch}"


class Fixture:
    def __init__(self, declaration=None, built=("alpha",), bin_targets=("alpha",),
                 with_packages=True, watch_mode=0o755):
        self.tmp = Path(tempfile.mkdtemp(prefix="rebuild-selfcont."))
        self.suf = host_suffix()
        self.repo = self.tmp / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        shutil.copy2(REBUILD, self.repo / "scripts" / "rebuild-plugins.sh")
        shutil.copy2(REPO / "scripts" / "cap-target-dir.sh", self.repo / "scripts" / "cap-target-dir.sh")
        core = self.repo / "crates" / "harness-core"
        core.mkdir(parents=True)
        (core / "Cargo.toml").write_text('[package]\nname = "harness-core"\nversion = "9.9.9"\n')
        cp = self.repo / "crates" / "alpha" / ".claude-plugin"
        cp.mkdir(parents=True)
        (cp / "plugin.json").write_text(json.dumps({"name": "alpha", "version": VER}) + "\n")
        if declaration is not None:
            (self.repo / "crates" / "alpha" / "self-contained-bins").write_text(declaration)
        env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t",
                   GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t")
        for cmd in (["git", "init", "-q"], ["git", "add", "."],
                    ["git", "-c", "commit.gpgsign=false", "commit", "-q", "--no-verify", "-m", "fixture"]):
            subprocess.run(cmd, cwd=self.repo, env=env, check=True, capture_output=True)

        self.target = self.tmp / "target"
        (self.target / "release").mkdir(parents=True)
        for name in built:
            f = self.target / "release" / name
            f.write_bytes(f"NEW-{name}\n".encode())
            f.chmod(0o755)

        meta = {"target_directory": str(self.target)}
        if with_packages:
            meta["packages"] = [{"name": "alpha", "targets": [
                {"name": n, "kind": ["bin"]} for n in bin_targets]}]
        shim = self.tmp / "shim"
        shim.mkdir()
        (self.tmp / "meta.json").write_text(json.dumps(meta, separators=(",", ":")) + "\n")
        cargo = shim / "cargo"
        cargo.write_text(f'#!/bin/sh\nif [ "$1" = metadata ]; then cat "{self.tmp}/meta.json"; fi\nexit 0\n')
        cargo.chmod(0o755)
        for tool, line in (("nm", "0000000100000000 T __mh_execute_header"),
                           ("file", "ELF 64-bit LSB executable, stripped")):
            stub = shim / tool
            stub.write_text(f"#!/bin/sh\necho '{line}'\n")
            stub.chmod(0o755)

        # FRESH version dir: launcher + self-contained script, NO host binaries.
        self.cache = self.tmp / "cache"
        self.bindir = self.cache / "alpha" / VER / "bin"
        self.bindir.mkdir(parents=True)
        (self.bindir / "alpha").write_text("#!/bin/sh\n")
        (self.bindir / "alpha").chmod(0o755)
        self.watch = self.bindir / "alpha-watch"
        self.watch.write_bytes(WATCH_BODY)
        self.watch.chmod(watch_mode)
        self.env = dict(os.environ, PATH=f"{shim}:{os.environ['PATH']}",
                        CARGO_TARGET_CAP_MB="0", CLAUDE_PLUGIN_CACHE=str(self.cache),
                        XDG_STATE_HOME=str(self.tmp / "state"))

    def run(self, *args):
        return subprocess.run(["bash", str(self.repo / "scripts" / "rebuild-plugins.sh"),
                               "--no-clean", "--only=alpha", *args],
                              cwd=self.repo, env=self.env, capture_output=True, text=True, timeout=120)

    def cleanup(self):
        shutil.rmtree(self.tmp, ignore_errors=True)


class SelfContainedBins(unittest.TestCase):
    def fx(self, **kw):
        f = Fixture(**kw)
        self.addCleanup(f.cleanup)
        return f

    def out(self, p):
        return f"\n--- stdout ---\n{p.stdout}\n--- stderr ---\n{p.stderr}"

    # 1
    def test_declared_self_contained_is_skipped_and_run_is_green(self):
        fx = self.fx(declaration="# self-contained scripts\n\n  alpha-watch  \n")
        p = fx.run()
        self.assertEqual(p.returncode, 0, self.out(p))
        self.assertEqual((fx.bindir / f"alpha-{fx.suf}").read_bytes(), b"NEW-alpha\n",
                         "control: the real launcher must still be seeded" + self.out(p))
        self.assertEqual(fx.watch.read_bytes(), WATCH_BODY)
        self.assertFalse((fx.bindir / f"alpha-watch-{fx.suf}").exists())
        self.assertTrue(any("self-contained" in l and "alpha-watch" in l for l in p.stdout.splitlines()),
                        "stdout must report the self-contained skip" + self.out(p))
        self.assertNotIn("could not seed", p.stderr)

    def test_declared_self_contained_dry_run_exits_zero(self):
        fx = self.fx(declaration="alpha-watch\n")
        p = fx.run("--dry-run")
        self.assertEqual(p.returncode, 0, self.out(p))
        self.assertFalse((fx.bindir / f"alpha-watch-{fx.suf}").exists())
        self.assertNotIn("could not seed", p.stderr)

    # 2
    def test_undeclared_script_still_errors(self):
        fx = self.fx(declaration=None)
        p = fx.run()
        self.assertNotEqual(p.returncode, 0, self.out(p))
        self.assertIn("could not seed a host binary into a FRESH version dir", p.stderr)
        self.assertIn("alpha/1.0.0:alpha-watch", p.stderr)

    # 3
    def test_declaration_does_not_excuse_real_launcher_missing_artifact(self):
        fx = self.fx(declaration="alpha-watch\n", built=())
        p = fx.run()
        self.assertNotEqual(p.returncode, 0, self.out(p))
        self.assertRegex(p.stderr, r"alpha/1\.0\.0:alpha(?![-\w])",
                         "the real launcher alpha must still be reported" + self.out(p))

    # 4
    def test_declaring_a_workspace_bin_target_is_contradictory(self):
        fx = self.fx(declaration="alpha-watch\nalpha\n")
        p = fx.run()
        self.assertNotEqual(p.returncode, 0, self.out(p))
        self.assertIn("ERROR", p.stderr)
        self.assertIn("alpha", p.stderr)

    def test_declared_name_listed_as_bin_target_in_metadata_is_contradictory(self):
        fx = self.fx(declaration="alpha-watch\n", bin_targets=("alpha", "alpha-watch"))
        p = fx.run()
        self.assertNotEqual(p.returncode, 0, self.out(p))
        self.assertIn("ERROR", p.stderr)
        self.assertIn("alpha-watch", p.stderr)

    # 5
    def test_declared_but_not_executable_errors(self):
        fx = self.fx(declaration="alpha-watch\n", watch_mode=0o644)
        p = fx.run()
        self.assertNotEqual(p.returncode, 0, self.out(p))
        self.assertIn("ERROR", p.stderr)
        self.assertIn("alpha-watch", p.stderr)

    # 6
    def test_malformed_declaration_line_errors(self):
        for bad in ("bin/alpha-watch\n", "alpha watch\n"):
            with self.subTest(bad=bad):
                fx = self.fx(declaration=bad)
                p = fx.run()
                self.assertNotEqual(p.returncode, 0, self.out(p))
                self.assertIn("ERROR", p.stderr)
                self.assertIn("self-contained-bins", p.stderr)

    # 7
    def test_declared_with_metadata_lacking_packages_errors(self):
        fx = self.fx(declaration="alpha-watch\n", with_packages=False)
        p = fx.run()
        self.assertNotEqual(p.returncode, 0, self.out(p))
        self.assertIn("ERROR", p.stderr)

    def test_nothing_declared_does_not_consult_packages(self):
        # control for 7: target_directory-only metadata keeps working when
        # nothing is declared (alpha-watch removed so only the launcher is in play).
        fx = self.fx(declaration=None, with_packages=False)
        fx.watch.unlink()
        p = fx.run()
        self.assertEqual(p.returncode, 0, self.out(p))


class RealRepo(unittest.TestCase):
    # 8
    def test_flow_declares_flow_watch_only(self):
        f = REPO / "crates" / "flow" / "self-contained-bins"
        self.assertTrue(f.is_file(), f"{f} missing")
        names = [l.strip() for l in f.read_text().splitlines()
                 if l.strip() and not l.strip().startswith("#")]
        self.assertIn("flow-watch", names)
        self.assertNotIn("flow", names)

    def test_flow_watch_tracked_executable(self):
        out = subprocess.run(["git", "ls-files", "-s", "crates/flow/bin/flow-watch"],
                             cwd=REPO, capture_output=True, text=True, check=True).stdout
        self.assertTrue(out.startswith("100755 "), f"git index entry: {out!r}")


if __name__ == "__main__":
    unittest.main(verbosity=2)
