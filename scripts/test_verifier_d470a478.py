#!/usr/bin/env python3
"""Independent verifier test for backlog d470a478 (written by the condukt
verifier, not by the implementer — CLAUDE.md section 2a).

Property: when lint-changed-crates.sh reports success, its output states the
lint scope and either names the workspace members that (transitively) depend on
a changed crate but were not linted, or says the dependent set is UNDETERMINED.
It must never claim "no workspace dependents" when the set could not be
computed, and the exit status must still follow the changed crates' fmt/clippy.

A stub `cargo` on PATH answers `metadata` with crafted JSON (or a failure) and
logs every other call, so the dependent graph is controlled exactly and no
toolchain is needed. Point at another script with LINT_CHANGED_CRATES_SCRIPT
(e.g. the pre-fix version) to observe it RED.

    python3 scripts/test_verifier_d470a478.py
"""

import json
import os
import shutil
import stat
import subprocess
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SCRIPT = Path(os.environ.get("LINT_CHANGED_CRATES_SCRIPT",
                              _HERE / "lint-changed-crates.sh")).resolve()
_CAP = _HERE / "cap-target-dir.sh"

# crate dir -> (package name, [(dep package name, kind)])
# core <- mid (normal) <- app (dev only); other is unrelated; reg-user depends
# on a NON-path dependency that happens to share core's name.
GRAPH = {
    "core": ("v-core", []),
    "mid": ("v-mid", [("v-core", None)]),
    "app": ("v-app", [("v-mid", "dev")]),
    "other": ("v-other", []),
    "reguser": ("v-reguser", [("v-core", "registry")]),
}

STUB = """#!/bin/sh
if [ "$1" = "metadata" ]; then
    case "$STUB_META_MODE" in
        fail) echo "stub metadata failure" >&2; exit 7 ;;
        garbage) echo "this is not json"; exit 0 ;;
        *) cat "{meta}"; exit 0 ;;
    esac
fi
echo "$*" >> "{log}"
if [ "$1" = "clippy" ] && [ -n "$STUB_CLIPPY_FAIL" ]; then exit 1; fi
exit 0
"""


def _git(cwd, *args):
    subprocess.run(["git", *args], cwd=cwd, check=True, capture_output=True)


def _metadata(repo):
    pkgs = []
    for d, (name, deps) in GRAPH.items():
        dl = []
        for dep, kind in deps:
            entry = {"name": dep, "kind": None if kind == "registry" else kind,
                     "source": None}
            if kind == "registry":
                entry["source"] = "registry+https://github.com/rust-lang/crates.io-index"
            else:
                entry["path"] = str(repo / "crates" / [k for k, v in GRAPH.items()
                                                       if v[0] == dep][0])
            dl.append(entry)
        pkgs.append({"name": name, "version": "0.0.0", "dependencies": dl,
                     "manifest_path": str(repo / "crates" / d / "Cargo.toml")})
    return {"packages": pkgs, "workspace_members": [], "version": 1}


class VerifierDependentScope(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="verif-d470a478."))
        self.addCleanup(shutil.rmtree, self.tmp, True)
        repo = self.tmp / "repo"
        (repo / "scripts").mkdir(parents=True)
        shutil.copy(_CAP, repo / "scripts" / "cap-target-dir.sh")
        for d, (name, _deps) in GRAPH.items():
            (repo / "crates" / d / "src").mkdir(parents=True)
            (repo / "crates" / d / "Cargo.toml").write_text(
                '[package]\nname = "%s"\nversion = "0.0.0"\n' % name)
            (repo / "crates" / d / "src" / "lib.rs").write_text("pub fn f() {}\n")
        _git(repo, "init", "-q")
        _git(repo, "config", "user.email", "t@t")
        _git(repo, "config", "user.name", "t")
        _git(repo, "add", "-A")
        _git(repo, "commit", "-qm", "init")
        self.repo = repo
        meta = self.tmp / "meta.json"
        meta.write_text(json.dumps(_metadata(repo)))
        self.log = self.tmp / "cargo.log"
        bindir = self.tmp / "bin"
        bindir.mkdir()
        stub = bindir / "cargo"
        stub.write_text(STUB.replace("{meta}", str(meta)).replace("{log}", str(self.log)))
        stub.chmod(stub.stat().st_mode | stat.S_IEXEC)
        self.env = dict(os.environ)
        self.env["PATH"] = str(bindir) + os.pathsep + "/usr/bin:/bin"
        for k in ("STUB_META_MODE", "STUB_CLIPPY_FAIL"):
            self.env.pop(k, None)

    def _touch(self, d):
        (self.repo / "crates" / d / "src" / "lib.rs").write_text(
            "pub fn f() { let _x = 1; }\n")

    def _run(self, **extra):
        env = dict(self.env, **extra)
        return subprocess.run(["bash", str(_SCRIPT)], cwd=self.repo, env=env,
                              capture_output=True, text=True)

    def _final_line(self, p):
        lines = [l for l in p.stdout.splitlines() if "all green" in l]
        self.assertEqual(1, len(lines), p.stdout + p.stderr)
        return lines[0].replace("lint-changed-crates:", "")

    def test_transitive_dependents_named_unrelated_and_registry_not(self):
        self._touch("core")
        p = self._run()
        self.assertEqual(0, p.returncode, p.stdout + p.stderr)
        self.assertIn("clippy -p v-core", self.log.read_text())
        line = self._final_line(p)
        self.assertIn("v-core", line)
        self.assertIn("v-mid", line)   # direct dependent
        self.assertIn("v-app", line)   # transitive, via dev-dependency
        self.assertNotIn("v-other", line)
        self.assertNotIn("v-reguser", line)  # registry dep, not a path dep
        self.assertNotIn("no workspace dependents", line)
        self.assertIn("NOT linted", line)

    def test_leaf_reports_no_dependents(self):
        self._touch("other")
        p = self._run()
        self.assertEqual(0, p.returncode, p.stdout + p.stderr)
        line = self._final_line(p)
        self.assertIn("v-other", line)
        self.assertIn("no workspace dependents", line)

    def test_metadata_exit_nonzero_is_undetermined(self):
        self._touch("other")  # a leaf: "no dependents" would be the tempting lie
        p = self._run(STUB_META_MODE="fail")
        self.assertEqual(0, p.returncode, p.stdout + p.stderr)
        line = self._final_line(p)
        self.assertIn("UNDETERMINED", line)
        self.assertNotIn("no workspace dependents", line)

    def test_metadata_garbage_is_undetermined(self):
        self._touch("other")
        p = self._run(STUB_META_MODE="garbage")
        self.assertEqual(0, p.returncode, p.stdout + p.stderr)
        line = self._final_line(p)
        self.assertIn("UNDETERMINED", line)
        self.assertNotIn("no workspace dependents", line)

    def test_clippy_failure_still_exits_nonzero(self):
        # Regression guard: the disclosure must not soften the verdict.
        self._touch("core")
        p = self._run(STUB_CLIPPY_FAIL="1", STUB_META_MODE="fail")
        self.assertNotEqual(0, p.returncode, p.stdout + p.stderr)
        self.assertNotIn("all green", p.stdout)


if __name__ == "__main__":
    unittest.main()
