#!/usr/bin/env python3
"""rollout-plugins.sh must refuse to deploy from a source tree that is not the committed HEAD.

Incident: the pre-push fail-open mutation (check-fail-open-mutation.py) rewrote
crates/blastguard/src/model.rs in place; a rollout running at that moment copied the
mutated source into the plugin cache and rebuilt + swapped in the mutated binary.

Contract pinned here (cases A-F):
  1. non-dry-run rollout with an uncommitted modification / untracked file under
     crates/, root Cargo.toml, Cargo.lock or .cargo/  -> exit 9, nothing deployed.
     Dirty files OUTSIDE those paths do not block (case E).
  2. <git-dir>/fail-open-mutation.active present -> exit 9 even on a clean tree.
     check-fail-open-mutation.py creates it (and bumps <git-dir>/fail-open-mutation.seq)
     BEFORE writing the mutated file and removes it only after the revert is confirmed.
  3. after cargo build and before any binary reaches the cache, the source is re-checked
     (clean, HEAD unchanged, .active absent, .seq unchanged); failure -> exit 9 and the
     mutated build output is not in the cache; the registry points where it did before.
  4. a clean tree still deploys (control).

Hermetic: same pattern as test_ea39fbc9_rollout_lock_verify.RolloutE2E (temp git repo
with a copy of scripts/, temp cache/registry/HOME/settings, fake cargo/nm/file first
on PATH). Nothing outside the temp root is touched.
"""
import importlib.util
import json
import os
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
EXIT_DIRTY = 9
TOKEN = "MUTATED_TOKEN_5f3a9c"
CLEAN_MARK = "CLEAN_SRC_MARK"

# Fake cargo. `metadata` answers the target dir. `build` writes target/release/p whose
# bytes embed the CURRENT content of crates/p/src/lib.rs (so a mutated source yields a
# binary carrying the mutation token), after optionally mutating the tree:
#   FAKE_BUILD_MODE=clean          no mutation
#   FAKE_BUILD_MODE=mutate         append TOKEN to lib.rs and leave it (tree stays dirty)
#   FAKE_BUILD_MODE=mutate_revert  bump <git-dir>/fail-open-mutation.seq, create .active,
#                                  append TOKEN, build, `git checkout` it back, drop .active
FAKE_CARGO = r"""#!/bin/sh
if [ "$1" = metadata ]; then echo '{"target_directory":"%(target)s"}'; exit 0; fi
if [ "$1" = build ]; then
  gd="$(git rev-parse --absolute-git-dir)"
  mode="${FAKE_BUILD_MODE:-clean}"
  src=crates/p/src/lib.rs
  case "$mode" in
    mutate_revert)
      n=0; [ -f "$gd/fail-open-mutation.seq" ] && n="$(cat "$gd/fail-open-mutation.seq")"
      echo $((n + 1)) > "$gd/fail-open-mutation.seq"
      : > "$gd/fail-open-mutation.active"
      echo "// %(token)s" >> "$src" ;;
    mutate) echo "// %(token)s" >> "$src" ;;
  esac
  mkdir -p "%(target)s/release"
  { printf 'FAKEBIN\n'; cat "$src"; } > "%(target)s/release/p"
  chmod +x "%(target)s/release/p"
  if [ "$mode" = mutate_revert ]; then
    git checkout -- "$src"
    rm -f "$gd/fail-open-mutation.active"
  fi
  exit 0
fi
echo "unexpected cargo $*" >&2
exit 1
"""


def _git(repo, *args, check=True):
    return subprocess.run(
        ["git", "-c", "user.email=a@b", "-c", "user.name=t", "-C", str(repo), *args],
        check=check, capture_output=True, text=True,
        env=dict(os.environ, GIT_CONFIG_GLOBAL="/dev/null"))


class Fixture(unittest.TestCase):
    def setUp(self):
        self.root = Path(tempfile.mkdtemp(prefix="rdirty-e2e-")).resolve()
        self.addCleanup(shutil.rmtree, self.root, True)
        repo = self.root / "repo"
        shutil.copytree(HERE, repo / "scripts", ignore=shutil.ignore_patterns("__pycache__"))
        crate = repo / "crates" / "p"
        (crate / ".claude-plugin").mkdir(parents=True)
        (crate / ".claude-plugin" / "plugin.json").write_text('{"name":"p","version":"3.0.0"}')
        (crate / "Cargo.toml").write_text('[package]\nname="p"\nversion="3.0.0"\n')
        (crate / "src").mkdir()
        (crate / "src" / "lib.rs").write_text(f"// {CLEAN_MARK}\n")
        (crate / "bin").mkdir()
        (crate / "bin" / "p").write_text("#!/bin/sh\nexit 0\n")
        (crate / "bin" / "p").chmod(0o755)
        (repo / "Cargo.toml").write_text('[workspace]\nmembers=["crates/p"]\n')
        (repo / "Cargo.lock").write_text("# lock\n")
        (repo / ".claude-plugin").mkdir()
        (repo / ".claude-plugin" / "marketplace.json").write_text(json.dumps(
            {"name": "yukineko", "owner": {"name": "y"}, "plugins": [
                {"name": "p", "version": "3.0.0",
                 "source": {"source": "git-subdir", "url": "x", "path": "crates/p", "ref": "main"}}]}))
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        _git(repo, "add", "-A")
        _git(repo, "update-index", "--chmod=+x", "crates/p/bin/p")
        _git(repo, "commit", "-qm", "i")
        self.repo = repo
        self.gitdir = repo / ".git"
        self.src = crate / "src" / "lib.rs"

        self.cache = self.root / "cache" / "yukineko"
        self.old = self.cache / "p" / "1.0.0"
        (self.old / "bin").mkdir(parents=True)
        self.new = self.cache / "p" / "3.0.0"
        self.registry = self.root / "reg.json"
        self.registry.write_text(json.dumps({"version": 2, "plugins": {"p@yukineko": [
            {"scope": "user", "installPath": str(self.old), "version": "1.0.0",
             "lastUpdated": "2026-01-01T00:00:00.000Z"}]}}))
        (self.root / "settings.json").write_text("{}")
        (self.root / "home").mkdir()
        fb = self.root / "fb"
        fb.mkdir()
        (fb / "cargo").write_text(FAKE_CARGO % {"target": str(self.root / "target"), "token": TOKEN})
        # rebuild-plugins.sh refuses unstripped artifacts; the fake artifact is not a real
        # Mach-O/ELF, so answer the strip probes (darwin: nm -a, linux: file -b) as "stripped".
        (fb / "nm").write_text("#!/bin/sh\nexit 0\n")
        (fb / "file").write_text("#!/bin/sh\necho 'ELF 64-bit LSB executable, stripped'\n")
        # HOME is a temp dir, so the real rustup has no toolchain; rebuild-plugins.sh only
        # needs `rustc -vV` for the host triple.
        triple = {"darwin": "aarch64-apple-darwin"}.get(sys.platform, "x86_64-unknown-linux-gnu")
        (fb / "rustc").write_text(f"#!/bin/sh\necho 'rustc 1.0.0'\necho 'host: {triple}'\n")
        for n in ("cargo", "nm", "file", "rustc"):
            (fb / n).chmod(0o755)
        self.env = dict(os.environ, HOME=str(self.root / "home"), CLAUDE_PLUGIN_CACHE=str(self.cache),
                        CLAUDE_PLUGIN_REGISTRY=str(self.registry),
                        CLAUDE_SETTINGS_JSON=str(self.root / "settings.json"),
                        PLUGIN_CACHE_PROC_LIST_PROBE="echo '1 1 python3'",
                        PATH=f"{fb}{os.pathsep}{os.environ['PATH']}")
        self.env.pop("HARNESS_ROLLOUT_LOCK_HOLDER_PID", None)
        self.env.pop("FAKE_BUILD_MODE", None)

    def run_rollout(self, mode=None):
        env = dict(self.env)
        if mode:
            env["FAKE_BUILD_MODE"] = mode
        return subprocess.run(["bash", str(self.repo / "scripts" / "rollout-plugins.sh")],
                              capture_output=True, text=True, env=env, cwd=str(self.repo), timeout=300)

    def registry_points(self):
        ents = json.loads(self.registry.read_text())["plugins"]["p@yukineko"]
        return [(e["version"], e["installPath"]) for e in ents]

    def cache_files_containing(self, token):
        hits = []
        for p in self.cache.rglob("*"):
            if p.is_file() and token.encode() in p.read_bytes():
                hits.append(str(p))
        return hits

    def assert_refused_nothing_deployed(self, r):
        out = f"rc={r.returncode}\n{r.stdout}{r.stderr}"
        self.assertEqual(r.returncode, EXIT_DIRTY, "expected exit 9 (dirty source)\n" + out)
        self.assertFalse(self.new.exists(), f"new version dir {self.new} was created\n" + out)
        self.assertEqual(self.registry_points(), [("1.0.0", str(self.old))], "registry moved\n" + out)
        self.assertEqual(self.cache_files_containing("FAKEBIN"), [], "a built binary reached the cache\n" + out)


class PreCheck(Fixture):
    def test_a_modified_tracked_file_in_crate_refuses(self):
        self.src.write_text(f"// {CLEAN_MARK}\n// local edit\n")
        self.assert_refused_nothing_deployed(self.run_rollout())

    def test_b_untracked_file_in_crate_refuses(self):
        (self.repo / "crates" / "p" / "src" / "extra.rs").write_text("// untracked\n")
        self.assert_refused_nothing_deployed(self.run_rollout())

    def test_c_active_mutation_marker_refuses_on_clean_tree(self):
        _git(self.repo, "diff", "--quiet", "--exit-code")  # tree is clean
        (self.gitdir / "fail-open-mutation.active").write_text("")
        self.assert_refused_nothing_deployed(self.run_rollout())

    def test_e_control_clean_tree_with_unrelated_untracked_file_deploys(self):
        (self.repo / "scratch.txt").write_text("not a build input\n")
        r = self.run_rollout()
        out = f"rc={r.returncode}\n{r.stdout}{r.stderr}"
        self.assertTrue(self.new.is_dir(), "control did not create the new version dir\n" + out)
        self.assertEqual(self.registry_points(), [("3.0.0", str(self.new))], "registry not repointed\n" + out)
        self.assertEqual(len(self.cache_files_containing(CLEAN_MARK)) >= 1, True,
                         "built binary was not swapped into the cache (UNVERIFIED control)\n" + out)
        self.assertNotEqual(r.returncode, EXIT_DIRTY, out)


class MidBuild(Fixture):
    def assert_mid_build_refused(self, r):
        out = f"rc={r.returncode}\n{r.stdout}{r.stderr}"
        self.assertEqual(r.returncode, EXIT_DIRTY, "expected exit 9 (source changed during build)\n" + out)
        self.assertEqual(self.cache_files_containing(TOKEN), [],
                         "MUTATED build output reached the plugin cache\n" + out)
        self.assertEqual(self.registry_points(), [("1.0.0", str(self.old))],
                         "registry does not point where it did before the run\n" + out)

    def test_d_source_mutated_during_build_is_not_deployed(self):
        self.assert_mid_build_refused(self.run_rollout("mutate"))

    def test_d2_mutate_then_revert_during_build_is_not_deployed(self):
        r = self.run_rollout("mutate_revert")
        # the fake build really did mutate+revert (tree is clean again, seq bumped)
        self.assertEqual(self.src.read_text(), f"// {CLEAN_MARK}\n")
        self.assertTrue((self.gitdir / "fail-open-mutation.seq").exists(), "fixture never bumped .seq")
        self.assert_mid_build_refused(r)


class MutationMarker(unittest.TestCase):
    def test_f_evaluate_scenario_brackets_mutation_with_marker_and_seq(self):
        root = Path(tempfile.mkdtemp(prefix="rdirty-f-")).resolve()
        self.addCleanup(shutil.rmtree, root, True)
        repo = root / "repo"
        (repo / "crates" / "g" / "src").mkdir(parents=True)
        f = repo / "crates" / "g" / "src" / "m.rs"
        f.write_text("fn a() { OLD }\n")
        subprocess.run(["git", "init", "-q", str(repo)], check=True)
        _git(repo, "add", "-A")
        _git(repo, "commit", "-qm", "i")

        spec = importlib.util.spec_from_file_location("fom_under_test", HERE / "check-fail-open-mutation.py")
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        gd = Path(subprocess.run(["git", "-C", str(repo), "rev-parse", "--git-dir"],
                                 capture_output=True, text=True, check=True).stdout.strip())
        gd = gd if gd.is_absolute() else repo / gd
        active, seq = gd / "fail-open-mutation.active", gd / "fail-open-mutation.seq"

        def seq_val():
            try:
                return int(seq.read_text().strip())
            except (OSError, ValueError):
                return 0

        before = seq_val()
        seen = {}

        def fake_tests(crate, timeout=600):
            seen["mutated_on_disk"] = "NEW" in f.read_text()
            seen["active"] = active.exists()
            seen["seq"] = seq_val()
            return 1, "test result: FAILED. 1 failed"

        mod.REPO = repo
        mod.run_crate_tests = fake_tests
        sc = mod.Scenario(crate="g", file_rel="crates/g/src/m.rs", description="d", old="OLD", new="NEW")
        res = mod.evaluate_scenario(sc, 5)

        self.assertTrue(seen.get("mutated_on_disk"), f"fixture: mutation not applied while tests ran ({res.status}: {res.detail})")
        self.assertTrue(seen["active"], ".active marker absent while the mutated file was on disk")
        self.assertGreater(seen["seq"], before, ".seq not bumped before the mutated file was written")
        self.assertTrue(res.revert_confirmed)
        self.assertEqual(f.read_text(), "fn a() { OLD }\n")
        self.assertFalse(active.exists(), ".active still present after confirmed revert")
        self.assertGreater(seq_val(), before, ".seq must stay bumped after revert")


if __name__ == "__main__":
    unittest.main()
