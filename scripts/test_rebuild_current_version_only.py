#!/usr/bin/env python3
"""IMPLEMENTER-WRITTEN tests for backlog 8acb117a (written by the implementer of
the fix, NOT by the independent RED author — weigh them accordingly; the
independent RED test is scripts/test_rebuild_preserves_superseded_versions.py).

Ruling under test: rebuild-plugins.sh writes ONLY the plugin's CURRENT version
dir (current = repo crates/<dir>/.claude-plugin/plugin.json "version", plugin
name -> crate dir via plugin.json "name"). Superseded version dirs stay
byte-for-byte untouched — binary, hooks config and .deployed-from.json alike.
A plugin whose current version cannot be determined is refreshed in NO dir and
the run exits non-zero (never "write every dir" as a fallback).

Also pins the verification side: check-plugin-rollout.py must accept a correctly
rolled-out cache whose superseded (live-held) dir still holds OLD bytes.

Hermetic: every rebuild case runs a COPY of the script inside a throwaway git
repo with fake crates, a temp CLAUDE_PLUGIN_CACHE, a fake target dir and a
`cargo` shim. The real cache and the real crates/ are never read or written.

REBUILD_SCRIPT=<path> overrides the script under test (used to run these cases
against mutants that remove the guard and observe them go RED).

Run: python3 scripts/test_rebuild_current_version_only.py
"""
import json
import os
import re
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
REBUILD = Path(os.environ.get("REBUILD_SCRIPT", REPO / "scripts" / "rebuild-plugins.sh"))

OLD_MANIFEST = '{"plugin":"x","commit":"old","dirty":false,"deployed_at":1}\n'


def host_suffix() -> str:
    out = subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout
    triple = re.search(r"^host: (.*)$", out, re.M).group(1)
    os_ = "darwin" if "apple-darwin" in triple else "linux" if "linux" in triple else "unknown"
    arch = "x86_64" if triple.startswith("x86_64-") else "arm64" if triple.startswith("aarch64-") else "unknown"
    return f"{os_}-{arch}"


# crate dir -> plugin.json contents. `run-book` deliberately differs from its
# plugin name `runbook`; `noversion` has no "version".
CRATES = {
    "alpha": {"name": "alpha", "version": "1.2.0"},
    "run-book": {"name": "runbook", "version": "0.3.0"},
    "noversion": {"name": "noversion"},
}
# plugin name -> version dirs present in the cache
CACHED = {
    "alpha": ["1.1.0", "1.2.0"],
    "runbook": ["0.2.0", "0.3.0"],
    "noversion": ["0.1.0"],
    "ghost": ["0.1.0"],  # no crate at all
}


class Fixture:
    def __init__(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="rebuild-curver."))
        self.suf = host_suffix()
        self.repo = self.tmp / "repo"
        (self.repo / "scripts").mkdir(parents=True)
        shutil.copy2(REBUILD, self.repo / "scripts" / "rebuild-plugins.sh")
        shutil.copy2(REPO / "scripts" / "cap-target-dir.sh", self.repo / "scripts" / "cap-target-dir.sh")
        core = self.repo / "crates" / "harness-core"
        core.mkdir(parents=True)
        (core / "Cargo.toml").write_text('[package]\nname = "harness-core"\nversion = "9.9.9"\n')
        for d, pj in CRATES.items():
            cp = self.repo / "crates" / d / ".claude-plugin"
            cp.mkdir(parents=True)
            (cp / "plugin.json").write_text(json.dumps(pj) + "\n")
            (self.repo / "crates" / d / "hooks").mkdir()
            (self.repo / "crates" / d / "hooks" / "hooks.json").write_text(f'{{"repo":"{d}"}}\n')
        env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t",
                   GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t")
        for cmd in (["git", "init", "-q"], ["git", "add", "."],
                    ["git", "-c", "commit.gpgsign=false", "commit", "-q", "--no-verify", "-m", "fixture"]):
            subprocess.run(cmd, cwd=self.repo, env=env, check=True, capture_output=True)

        self.target = self.tmp / "target"
        (self.target / "release").mkdir(parents=True)
        for name in ("alpha", "runbook", "noversion", "ghost"):
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
        self.env = dict(os.environ, PATH=f"{shim}:{os.environ['PATH']}", FAKE_TARGET=str(self.target),
                        CARGO_TARGET_CAP_MB="0")

        self.cache = self.tmp / "cache"
        for name, vers in CACHED.items():
            for v in vers:
                vd = self.cache / name / v
                (vd / "bin").mkdir(parents=True)
                (vd / "bin" / name).write_text("#!/bin/sh\n")  # launcher
                b = vd / "bin" / f"{name}-{self.suf}"
                b.write_bytes(f"OLD-{name}-{v}\n".encode())
                b.chmod(0o755)
                (vd / "hooks").mkdir()
                (vd / "hooks" / "hooks.json").write_text(f'{{"stale":"{v}"}}\n')
                (vd / ".deployed-from.json").write_text(OLD_MANIFEST)
        self.env["CLAUDE_PLUGIN_CACHE"] = str(self.cache)

    def snapshot(self, name, ver):
        vd = self.cache / name / ver
        return {str(p.relative_to(vd)): p.read_bytes() for p in sorted(vd.rglob("*")) if p.is_file()}

    def run(self, *args):
        return subprocess.run(["bash", str(self.repo / "scripts" / "rebuild-plugins.sh"), "--no-clean", *args],
                              cwd=self.repo, env=self.env, capture_output=True, text=True)

    def cleanup(self):
        shutil.rmtree(self.tmp, ignore_errors=True)


class CurrentVersionOnly(unittest.TestCase):
    def setUp(self):
        self.fx = Fixture()
        self.addCleanup(self.fx.cleanup)

    def test_superseded_dir_is_byte_identical_including_provenance(self):
        before = self.fx.snapshot("alpha", "1.1.0")
        p = self.fx.run("--only=alpha")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(self.fx.snapshot("alpha", "1.1.0"), before,
                         "superseded alpha/1.1.0 was modified (binary, hooks or .deployed-from.json)\n" + p.stdout)
        # control: the current dir IS refreshed, and its provenance rewritten
        cur = self.fx.cache / "alpha" / "1.2.0"
        self.assertEqual((cur / "bin" / f"alpha-{self.fx.suf}").read_bytes(), b"NEW-alpha\n")
        self.assertEqual((cur / "hooks" / "hooks.json").read_text(), '{"repo":"alpha"}\n')
        self.assertEqual(json.loads((cur / ".deployed-from.json").read_text())["plugin"], "alpha")

    def test_renamed_crate_dir_resolves_current_via_plugin_json_name(self):
        before = self.fx.snapshot("runbook", "0.2.0")
        p = self.fx.run("--only=runbook")
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual(self.fx.snapshot("runbook", "0.2.0"), before, p.stdout)
        cur = self.fx.cache / "runbook" / "0.3.0"
        self.assertEqual((cur / "bin" / f"runbook-{self.fx.suf}").read_bytes(), b"NEW-runbook\n")
        self.assertEqual((cur / "hooks" / "hooks.json").read_text(), '{"repo":"run-book"}\n')

    def _assert_undetermined_fails_closed(self, name, ver):
        before = self.fx.snapshot(name, ver)
        alpha_old = self.fx.snapshot("alpha", "1.1.0")
        p = self.fx.run(f"--only={name},alpha")
        self.assertNotEqual(p.returncode, 0,
                            f"undetermined current version for {name} must exit non-zero\n{p.stdout}{p.stderr}")
        self.assertIn(name, p.stderr)
        self.assertIn("cannot determine the CURRENT version", p.stderr)
        self.assertEqual(self.fx.snapshot(name, ver), before,
                         f"{name}/{ver} was written although its current version is unknown\n{p.stdout}")
        # not over-blocking: other plugins in the same run are still handled correctly
        self.assertEqual((self.fx.cache / "alpha" / "1.2.0" / "bin" / f"alpha-{self.fx.suf}").read_bytes(),
                         b"NEW-alpha\n")
        self.assertEqual(self.fx.snapshot("alpha", "1.1.0"), alpha_old)

    def test_cached_plugin_with_no_crate_fails_closed(self):
        self._assert_undetermined_fails_closed("ghost", "0.1.0")

    def test_plugin_json_without_version_fails_closed(self):
        self._assert_undetermined_fails_closed("noversion", "0.1.0")

    def test_unfiltered_run_also_freezes_superseded(self):
        """No --only (manual/standalone call) must obey the same rule."""
        before = {(n, v): self.fx.snapshot(n, v) for n, v in (("alpha", "1.1.0"), ("runbook", "0.2.0"))}
        p = self.fx.run()
        # ghost/noversion are in the cache, so the run is non-zero by design
        self.assertNotEqual(p.returncode, 0, p.stdout + p.stderr)
        for key, snap in before.items():
            self.assertEqual(self.fx.snapshot(*key), snap, f"{key} modified\n{p.stdout}")


SYNC_SCRIPTS = sorted(REPO.glob("crates/*/scripts/sync-plugin-assets.sh"))


class SyncAssetsCurrentVersionOnly(unittest.TestCase):
    """The other writer into version dirs: each plugin's sync-plugin-assets.sh
    must mirror skills/agents/hooks into the CURRENT version dir only.
    SYNC_SCRIPT=<path> overrides the script (for mutants)."""

    def _check(self, script):
        tmp = Path(tempfile.mkdtemp(prefix="sync-curver."))
        self.addCleanup(shutil.rmtree, tmp, ignore_errors=True)
        plug = tmp / "plugin"
        (plug / "scripts").mkdir(parents=True)
        shutil.copy2(script, plug / "scripts" / "sync-plugin-assets.sh")
        (plug / ".claude-plugin").mkdir()
        (plug / ".claude-plugin" / "plugin.json").write_text('{"name": "p", "version": "2.0.0"}\n')
        (plug / "skills").mkdir()
        (plug / "skills" / "x.md").write_text("NEW\n")
        cache_root = tmp / "cache"
        for v in ("1.0.0", "2.0.0"):
            d = cache_root / "yukineko" / "p" / v / "skills"
            d.mkdir(parents=True)
            (d / "x.md").write_text(f"OLD-{v}\n")
        p = subprocess.run(["bash", str(plug / "scripts" / "sync-plugin-assets.sh")],
                           env=dict(os.environ, CLAUDE_PLUGIN_CACHE=str(cache_root)),
                           capture_output=True, text=True)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertEqual((cache_root / "yukineko/p/1.0.0/skills/x.md").read_text(), "OLD-1.0.0\n",
                         "superseded p/1.0.0 was synced")
        self.assertEqual((cache_root / "yukineko/p/2.0.0/skills/x.md").read_text(), "NEW\n")

    def test_every_sync_script_writes_current_version_only(self):
        scripts = [Path(os.environ["SYNC_SCRIPT"])] if os.environ.get("SYNC_SCRIPT") else SYNC_SCRIPTS
        self.assertTrue(scripts, "no crates/*/scripts/sync-plugin-assets.sh found — test would be vacuous")
        for s in scripts:
            with self.subTest(script=str(s)):
                self._check(s)


class CheckerAcceptsFrozenSuperseded(unittest.TestCase):
    """check-plugin-rollout.py must NOT demand that a superseded dir match the
    current build. Built on test_check_plugin_rollout's fixture: a live-held
    superseded condukt dir is given an OLD host binary, OLD hooks and an OLD
    provenance manifest whose harness_core_version is stale."""

    STALE_MANIFEST = {"commit": "deadbeef" * 5, "dirty": True, "harness_core_version": "0.0.1",
                      "deployed_at": 0}

    def _run(self, where):
        sys.path.insert(0, str(REPO / "scripts"))
        try:
            import test_check_plugin_rollout as tcpr
        finally:
            sys.path.pop(0)
        orig = tcpr._make_fixture
        suf = tcpr._HOST_SUFFIX
        stale = self.STALE_MANIFEST

        def patched(tmp, **kw):
            res = orig(tmp, **kw)
            cache = Path(tmp) / "cache" / "condukt"
            vd = cache / ("0.0.1" if where == "superseded" else tcpr.FIXTURE_PLUGINS["condukt"])
            (vd / "bin").mkdir(parents=True, exist_ok=True)
            b = vd / "bin" / f"condukt-{suf}"
            b.write_bytes(b"#!/bin/sh\necho OLD\n")
            b.chmod(0o755)
            (vd / "hooks").mkdir(exist_ok=True)
            (vd / "hooks" / "hooks.json").write_text('{"old":true}\n')
            (vd / ".deployed-from.json").write_text(json.dumps(stale))
            return res

        tcpr._make_fixture = patched
        try:
            case = tcpr._FixtureCase()
            with tempfile.TemporaryDirectory() as tmp:
                return case.run_main(tmp, cached_versions={"condukt": [("0.0.1", (os.getpid(),))]})
        finally:
            tcpr._make_fixture = orig

    def test_old_bytes_in_live_held_superseded_dir_pass(self):
        rc, out, err = self._run("superseded")
        self.assertEqual(rc, 0, f"checker demanded a superseded dir match the build\nout={out}\nerr={err}")

    def test_control_same_old_bytes_in_current_dir_are_red(self):
        """Proves the stale fixture is detectable at all, so the green above is
        not vacuous."""
        rc, out, err = self._run("current")
        self.assertNotEqual(rc, 0, f"stale manifest in the CURRENT dir went unnoticed\nout={out}\nerr={err}")


if __name__ == "__main__":
    unittest.main()
