#!/usr/bin/env python3
"""VERIFIER-WRITTEN (independent of the a0525604 implementer).

Case not covered by the pin (which is --dry-run only) nor by the worker's
test_rebuild_zero_artifacts_not_green.py (which always reports a valid
target_directory): a REAL run where cargo metadata exits 0 with no
target_directory AND REPO/target/release holds STALE binaries for every
plugin. The old fallback to REPO/target then deployed those stale bytes into
the cache and exited 0 -- the zero-artifact counter cannot catch this
(artifacts are found), only the empty-metadata guard can.

Also asserts the guard fires BEFORE cargo build and before any cache write.
Hermetic: script copy in a temp git repo, temp CLAUDE_PLUGIN_CACHE, cargo shim.
REBUILD_SCRIPT=path runs it against another revision of rebuild-plugins.sh.
"""
import json, os, re, shutil, subprocess, tempfile, unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
REBUILD = Path(os.environ.get("REBUILD_SCRIPT", REPO / "scripts" / "rebuild-plugins.sh"))
CAP = REBUILD.parent / "cap-target-dir.sh"
PLUGINS = {"alpha": "1.0.0", "beta": "2.0.0"}
SHIM = "#!/bin/sh\necho \"$*\" >> \"LOGFILE\"\nif [ \"$1\" = metadata ]; then echo '{\"packages\":[],\"workspace_root\":\"/x\"}'; fi\nexit 0\n"


def suffix():
    t = re.search(r"^host: (.*)$", subprocess.run(["rustc", "-vV"], capture_output=True, text=True, check=True).stdout, re.M).group(1)
    o = "darwin" if "apple-darwin" in t else "linux" if "linux" in t else "unknown"
    a = "x86_64" if t.startswith("x86_64-") else "arm64" if t.startswith("aarch64-") else "unknown"
    return o + "-" + a


class EmptyMetadataRealRun(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="verif-a0525604."))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.suf = suffix()
        repo = self.repo = self.tmp / "repo"
        (repo / "scripts").mkdir(parents=True)
        shutil.copy2(REBUILD, repo / "scripts" / "rebuild-plugins.sh")
        shutil.copy2(CAP, repo / "scripts" / "cap-target-dir.sh")
        (repo / "crates" / "harness-core").mkdir(parents=True)
        (repo / "crates" / "harness-core" / "Cargo.toml").write_text('[package]\nname="harness-core"\nversion="9.9.9"\n')
        for n, v in PLUGINS.items():
            d = repo / "crates" / n / ".claude-plugin"; d.mkdir(parents=True)
            (d / "plugin.json").write_text(json.dumps({"name": n, "version": v}))
        env = dict(os.environ, GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@t", GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@t")
        for c in (["git", "init", "-q"], ["git", "add", "."], ["git", "-c", "commit.gpgsign=false", "commit", "-q", "-m", "f"]):
            subprocess.run(c, cwd=repo, env=env, check=True, capture_output=True)
        rel = repo / "target" / "release"; rel.mkdir(parents=True)
        for n in PLUGINS:
            (rel / n).write_bytes(("STALE-" + n + "\n").encode()); (rel / n).chmod(0o755)
        shim = self.tmp / "shim"; shim.mkdir()
        self.log = self.tmp / "cargo.log"
        (shim / "cargo").write_text(SHIM.replace("LOGFILE", str(self.log)))
        (shim / "cargo").chmod(0o755)
        self.cache = self.tmp / "cache"
        for n, v in PLUGINS.items():
            b = self.cache / n / v / "bin"; b.mkdir(parents=True)
            (b / n).write_text("#!/bin/sh\n")
            hb = b / (n + "-" + self.suf); hb.write_bytes(("OLD-" + n + "\n").encode()); hb.chmod(0o755)
        self.env = dict(os.environ, PATH=str(shim) + ":" + os.environ["PATH"], CARGO_TARGET_CAP_MB="0",
                        CLAUDE_PLUGIN_CACHE=str(self.cache))

    def test_empty_metadata_real_run_does_not_deploy_stale_guess(self):
        p = subprocess.run(["bash", str(self.repo / "scripts" / "rebuild-plugins.sh"), "--no-clean"],
                           cwd=self.repo, env=self.env, capture_output=True, text=True, timeout=120)
        self.assertNotEqual(p.returncode, 0, "exited 0 on empty metadata:\n" + p.stdout + p.stderr)
        for n, v in PLUGINS.items():
            got = (self.cache / n / v / "bin" / (n + "-" + self.suf)).read_bytes()
            self.assertEqual(got, ("OLD-" + n + "\n").encode(), n + ": stale REPO/target bytes deployed")
            self.assertFalse((self.cache / n / v / ".deployed-from.json").exists(), n + ": provenance written")
        calls = self.log.read_text() if self.log.exists() else ""
        self.assertNotIn("build", calls, "cargo build ran despite unknown build dir: " + repr(calls))
        self.assertIn("target_directory", p.stderr, "error must name the missing field")


if __name__ == "__main__":
    unittest.main(verbosity=2)
