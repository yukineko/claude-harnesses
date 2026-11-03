#!/usr/bin/env python3
"""Black-box tests for the input-keyed pass cache of scripts/check-compile-fail.py
(spec C1-C9; C10 pre-push is not covered here).

Run: `python3 scripts/test_check_compile_fail_cache.py` (stdlib `unittest` only).

Set CFC_SCRIPT_UNDER_TEST to point the suite at another copy of the script (the
RED run uses main's version). The script is copied into a throwaway git repo
with a fake workspace; `cargo` and `rustc` are stubs on PATH, so no real build
happens. The stub cargo logs every `cargo test` call, which is the observable
that tells "ran" from "skipped".
"""

from __future__ import annotations

import json
import os
import re
import shutil
import stat
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

HERE = Path(__file__).resolve().parent
DEFAULT_SCRIPT = HERE / "check-compile-fail.py"
SCRIPT_SRC = Path(os.environ.get("CFC_SCRIPT_UNDER_TEST", str(DEFAULT_SCRIPT)))
SCRATCH = Path(
    "/private/tmp/claude-502/-Users-yuki-src-harness/6d697f68-2bc8-4991-8bfc-1c28d73edbc9/scratchpad"
)

LABELS = [
    "harness-core::verdict_compile_fail",
    "harness-core::scope_compile_fail",
    "blastguard::verdict_compile_fail",
    "donegate::verdict_compile_fail",
]
CRATES = ["harness-core", "blastguard", "donegate", "overwatch"]

CARGO_STUB = r'''#!__PY__
import json, os, sys

args = sys.argv[1:]
fixture = os.environ["CFC_FIXTURE"]
log = os.environ["CFC_LOG"]

if args[:1] == ["-V"] or args[:1] == ["--version"]:
    print("cargo 1.90.0 (abc123 2026-01-01)")
    sys.exit(0)

if args[:1] == ["metadata"]:
    mode = os.environ.get("CFC_META", "ok")
    if mode == "exit1":
        sys.stderr.write("error: metadata failed\n")
        sys.exit(1)
    if mode == "garbage":
        print("this is {{ not json")
        sys.exit(0)
    deps = {
        "harness-core": [],
        "overwatch": [("harness-core", None)],
        "blastguard": [("harness-core", None), ("overwatch", None)],
        "donegate": [("harness-core", None), ("overwatch", "dev")],
    }
    names = list(deps)
    if mode == "missing":
        names.remove("donegate")
    def pid(n):
        return "path+file://%s/crates/%s#0.1.0" % (fixture, n)
    REG_SRC = "registry+https://github.com/rust-lang/crates.io-index"
    def rid(n, v):
        return "%s#%s@%s" % (REG_SRC, n, v)
    reg = json.loads(os.environ.get("CFC_REG", "null")) or {
        "deps": {"harness-core": [["serde", "1.0.1"]], "donegate": [["itoa", "1.0.0"]]},
        "unrelated": [["fde", "0.1.0"]],
    }
    reg_pkgs = {}
    for owner in names:
        for rn, rv in reg["deps"].get(owner, []):
            reg_pkgs[rid(rn, rv)] = (rn, rv)
    for rn, rv in reg.get("unrelated", []):
        reg_pkgs[rid(rn, rv)] = (rn, rv)
    packages = []
    nodes = []
    for n in names:
        packages.append({
            "name": n, "version": "0.1.0", "id": pid(n), "source": None,
            "manifest_path": "%s/crates/%s/Cargo.toml" % (fixture, n),
            "dependencies": [
                {"name": d, "source": None, "req": "*", "kind": k,
                 "path": "%s/crates/%s" % (fixture, d)}
                for d, k in deps[n]
            ],
        })
        nodes.append({
            "id": pid(n),
            "dependencies": [pid(d) for d, _ in deps[n]],
            "deps": [
                {"name": d.replace("-", "_"), "pkg": pid(d),
                 "dep_kinds": [{"kind": k, "target": None}]}
                for d, k in deps[n]
            ],
            "features": [],
        })
    for n in names:
        for rn, rv in reg["deps"].get(n, []):
            for pk in packages:
                if pk["name"] == n:
                    pk["dependencies"].append(
                        {"name": rn, "source": REG_SRC, "req": "*", "kind": None})
            for nd in nodes:
                if nd["id"] == pid(n):
                    nd["dependencies"].append(rid(rn, rv))
                    nd["deps"].append(
                        {"name": rn, "pkg": rid(rn, rv),
                         "dep_kinds": [{"kind": None, "target": None}]})
    for i, (k, (rn, rv)) in enumerate(sorted(reg_pkgs.items())):
        packages.append({
            "name": rn, "version": rv, "id": k, "source": REG_SRC,
            "manifest_path": "/home/u/.cargo/registry/src/x/%s-%s/Cargo.toml" % (rn, rv),
            "dependencies": [],
        })
        node = {"id": k, "dependencies": [], "deps": [], "features": []}
        if rn == "itoa" and mode == "regnode_missing":
            continue
        if rn == "itoa" and mode == "regid_bad":
            del node["id"]
        nodes.append(node)
    print(json.dumps({
        "packages": packages,
        "workspace_members": [pid(n) for n in names],
        "resolve": {"nodes": nodes, "root": None},
        "target_directory": fixture + "/target",
        "version": 1,
        "workspace_root": fixture,
    }))
    sys.exit(0)

if args[:1] == ["test"]:
    pkg = args[args.index("-p") + 1]
    test = args[args.index("--test") + 1]
    label = "%s::%s" % (pkg, test)
    with open(log, "a") as f:
        f.write("%s TRYBUILD=%d\n" % (label, 1 if "TRYBUILD" in os.environ else 0))
    if label in os.environ.get("CFC_ABNORMAL", "").split(","):
        sys.stderr.write("error: could not compile `%s`\n" % pkg)
        sys.exit(101)
    if label in os.environ.get("CFC_FAIL", "").split(","):
        print("test result: FAILED. 0 passed; 1 failed; 0 ignored; 0 measured; 0 filtered out")
        sys.exit(101)
    print("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out")
    sys.exit(0)

sys.stderr.write("stub cargo: unexpected args %r\n" % (args,))
sys.exit(99)
'''

RUSTC_STUB = r'''#!__PY__
import os, sys
ver = os.environ.get("CFC_RUSTC_VERSION", "1.90.0")
extra = os.environ.get("CFC_RUSTC_EXTRA", "")
first = "rustc %s (abc123 2026-01-01)" % ver
if "-vV" in sys.argv[1:] or "--verbose" in sys.argv[1:]:
    print(first)
    print("binary: rustc")
    print("commit-hash: abc123")
    print("host: x86_64-unknown-linux-gnu")
    print("release: %s" % ver)
    if extra:
        print("extra: %s" % extra)
else:
    print(first)
sys.exit(0)
'''


def _git(cwd: Path, *args: str) -> str:
    r = subprocess.run(
        ["git", *args], cwd=cwd, capture_output=True, text=True, check=True
    )
    return r.stdout


@unittest.skipIf(sys.version_info < (3, 11), "script needs tomllib (python >= 3.11)")
class CacheBase(unittest.TestCase):
    def setUp(self) -> None:
        base = SCRATCH if SCRATCH.is_dir() else None
        self.tmp = Path(tempfile.mkdtemp(prefix="cfc-", dir=base)).resolve()
        self.addCleanup(self._cleanup)
        self.fix = self.tmp / "repo"
        self.bin = self.tmp / "bin"
        self.log = self.tmp / "cargo.log"
        self.cache = self.tmp / "cache.json"
        self.bin.mkdir()
        for name, body in (("cargo", CARGO_STUB), ("rustc", RUSTC_STUB)):
            p = self.bin / name
            p.write_text(body.replace("__PY__", sys.executable))
            p.chmod(p.stat().st_mode | stat.S_IXUSR | stat.S_IXGRP | stat.S_IXOTH)
        self._build_fixture()

    def _cleanup(self) -> None:
        for dp, dns, _ in os.walk(self.tmp):
            for d in dns:
                try:
                    os.chmod(Path(dp) / d, 0o755)
                except OSError:
                    pass
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _build_fixture(self) -> None:
        f = self.fix
        f.mkdir()
        (f / "scripts").mkdir()
        shutil.copy(SCRIPT_SRC, f / "scripts" / "check-compile-fail.py")
        (f / "Cargo.toml").write_text('[workspace]\nmembers = ["crates/*"]\n')
        (f / "Cargo.lock").write_text("# lock v1\n")
        (f / "rust-toolchain.toml").write_text(
            '# pin\n[toolchain]\nchannel = "1.90.0"\n'
        )
        for c in CRATES:
            (f / "crates" / c / "src").mkdir(parents=True)
            (f / "crates" / c / "src" / "lib.rs").write_text("// %s v1\n" % c)
            (f / "crates" / c / "Cargo.toml").write_text(
                '[package]\nname = "%s"\nversion = "0.1.0"\n' % c
            )
        _git(f, "init", "-q")
        _git(f, "config", "user.name", "t")
        _git(f, "config", "user.email", "t@example.com")
        _git(f, "config", "commit.gpgsign", "false")
        _git(f, "add", "-A")
        _git(f, "commit", "-q", "-m", "init")

    # ---- helpers -------------------------------------------------------
    def commit(self, rel: str, text: str, mode: str = "w") -> None:
        p = self.fix / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        with open(p, mode) as fh:
            fh.write(text)
        _git(self.fix, "add", "-A")
        _git(self.fix, "commit", "-q", "-m", "change " + rel)

    def run_script(self, **kw):
        """Run the script; returns (rc, stderr, ran_labels)."""
        env = {
            k: v
            for k, v in os.environ.items()
            if not k.startswith("CFC_") and not k.startswith("CHECK_COMPILE_FAIL")
            and k != "TRYBUILD"
        }
        env["PATH"] = str(self.bin) + os.pathsep + env.get("PATH", "")
        env["CFC_FIXTURE"] = str(self.fix)
        env["CFC_LOG"] = str(self.log)
        env["CHECK_COMPILE_FAIL_CACHE"] = str(kw.pop("cache", self.cache))
        for k, v in kw.pop("env", {}).items():
            env[k] = v
        if self.log.exists():
            self.log.unlink()
        r = subprocess.run(
            [sys.executable, str(self.fix / "scripts" / "check-compile-fail.py")],
            cwd=self.fix,
            env=env,
            capture_output=True,
            text=True,
        )
        ran = []
        if self.log.exists():
            ran = [ln.split()[0] for ln in self.log.read_text().splitlines() if ln]
        self.last_log = self.log.read_text() if self.log.exists() else ""
        return r.returncode, r.stderr, ran

    def prime(self) -> None:
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertEqual(sorted(ran), sorted(LABELS), err)

    @staticmethod
    def skipped(err: str) -> dict:
        out = {}
        for ln in err.splitlines():
            m = re.fullmatch(
                r"check-compile-fail: skipped (\S+) "
                r"\(inputs unchanged since a pass, key ([0-9a-f]{12})\)",
                ln,
            )
            if m:
                out[m.group(1)] = m.group(2)
        return out

    def assertRanOnly(self, ran, expected, err=""):
        self.assertEqual(sorted(ran), sorted(expected), err)
        self.assertEqual(set(self.skipped(err)), set(LABELS) - set(expected), err)

    def entries(self) -> dict:
        return json.loads(self.cache.read_text())["entries"]


class TestSkipAndKeyScope(CacheBase):
    def test_c2_second_run_skips_everything(self):
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4, err)
        self.assertNotIn("cache not used", err)  # missing cache file is silent
        self.assertEqual(self.skipped(err), {})
        # same fixture, only the cache state differs now
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertEqual(ran, [], err)
        sk = self.skipped(err)
        self.assertEqual(sorted(sk), sorted(LABELS), err)
        for label, short in sk.items():
            self.assertEqual(self.entries()[label][:12], short)
        self.assertEqual(
            err.strip().splitlines()[-1],
            "check-compile-fail: all 4 trybuild targets passed (4 from cache)",
        )

    def test_c7_final_line_unchanged_without_cache_hits(self):
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0)
        self.assertEqual(
            err.strip().splitlines()[-1],
            "check-compile-fail: all 4 trybuild targets passed",
        )

    def test_c1_donegate_change_reruns_only_donegate(self):
        self.prime()
        self.commit("crates/donegate/src/lib.rs", "// more\n", "a")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, ["donegate::verdict_compile_fail"], err)
        self.assertIn("(3 from cache)", err)

    def test_c1_harness_core_change_reruns_all(self):
        self.prime()
        self.commit("crates/harness-core/src/lib.rs", "// more\n", "a")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, LABELS, err)

    def test_c1_overwatch_change_reruns_dependents_only(self):
        self.prime()
        self.commit("crates/overwatch/src/lib.rs", "// more\n", "a")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(
            ran,
            ["blastguard::verdict_compile_fail", "donegate::verdict_compile_fail"],
            err,
        )

    # c1_cargo_lock_change_reruns_all was replaced ON PURPOSE by the spec change
    # cfc-lock-spec.md L1-L6 (user ruling 2026-11-03): Cargo.lock is no longer a
    # whole-file key input; the key carries the resolved ids of the dependency
    # closure instead. The three tests below are its replacement, not a weakening.
    def test_l3_cargo_lock_edit_outside_closure_skips_all(self):
        self.prime()
        self.commit("Cargo.lock", "# lock v1\n[[package]]\nname = \"fde\"\n", "a")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, [], err)
        self.assertIn("(4 from cache)", err)

    def test_l4a_registry_dep_change_in_donegate_closure_reruns_donegate_only(self):
        self.prime()
        self.commit("Cargo.lock", "# lock v2 itoa 1.0.1\n")
        reg = {
            "deps": {"harness-core": [["serde", "1.0.1"]], "donegate": [["itoa", "1.0.1"]]},
            "unrelated": [["fde", "0.1.0"]],
        }
        rc, err, ran = self.run_script(env={"CFC_REG": json.dumps(reg)})
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, ["donegate::verdict_compile_fail"], err)

    def test_l4b_registry_dep_change_in_harness_core_closure_reruns_all(self):
        self.prime()
        self.commit("Cargo.lock", "# lock v2 serde 1.0.2\n")
        reg = {
            "deps": {"harness-core": [["serde", "1.0.2"]], "donegate": [["itoa", "1.0.0"]]},
            "unrelated": [["fde", "0.1.0"]],
        }
        rc, err, ran = self.run_script(env={"CFC_REG": json.dumps(reg)})
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, LABELS, err)

    def test_l2_closure_id_change_alone_changes_key_without_lock_edit(self):
        # The key comes from the resolved ids in cargo metadata, not from the
        # Cargo.lock file: no Cargo.lock commit here, yet donegate must rerun.
        self.prime()
        reg = {
            "deps": {"harness-core": [["serde", "1.0.1"]], "donegate": [["itoa", "1.0.1"]]},
            "unrelated": [["fde", "0.1.0"]],
        }
        rc, err, ran = self.run_script(env={"CFC_REG": json.dumps(reg)})
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, ["donegate::verdict_compile_fail"], err)

    def test_l2_unrelated_registry_package_change_skips_all(self):
        self.prime()
        reg = {
            "deps": {"harness-core": [["serde", "1.0.1"]], "donegate": [["itoa", "1.0.0"]]},
            "unrelated": [["fde", "0.2.0"], ["other", "9.9.9"]],
        }
        rc, err, ran = self.run_script(env={"CFC_REG": json.dumps(reg)})
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, [], err)

    def test_c1_toolchain_file_change_reruns_all(self):
        self.prime()
        self.commit("rust-toolchain.toml", "# touched\n", "a")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, LABELS, err)

    def test_c1_root_cargo_toml_change_reruns_all(self):
        self.prime()
        self.commit("Cargo.toml", "# touched\n", "a")
        _, err, ran = self.run_script()
        self.assertRanOnly(ran, LABELS, err)

    def test_c1_script_change_reruns_all(self):
        self.prime()
        self.commit("scripts/check-compile-fail.py", "\n# touched\n", "a")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, LABELS, err)

    def test_c1_unrelated_crate_reruns_none(self):
        self.prime()
        self.commit("crates/other/src/lib.rs", "// other\n")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, [], err)

    def test_c1_different_rustc_vv_reruns_all(self):
        self.prime()
        rc, err, ran = self.run_script(env={"CFC_RUSTC_EXTRA": "other-build"})
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, LABELS, err)

    def test_c1_dirty_edit_runs_and_names_target(self):
        self.prime()
        (self.fix / "crates/donegate/src/lib.rs").write_text("// uncommitted\n")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, ["donegate::verdict_compile_fail"], err)
        self.assertTrue(
            any(
                ln.startswith("check-compile-fail: cache not used for ")
                and "donegate::verdict_compile_fail" in ln
                for ln in err.splitlines()
            ),
            err,
        )
        # dirty runs must not record: clean again (restore) -> skip returns
        _git(self.fix, "checkout", "--", "crates/donegate/src/lib.rs")
        rc, err, ran = self.run_script()
        self.assertEqual(ran, [], err)

    def test_c1_untracked_file_runs_and_names_target(self):
        self.prime()
        (self.fix / "crates/donegate/src/new.rs").write_text("// new\n")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, ["donegate::verdict_compile_fail"], err)
        self.assertTrue(
            any(
                ln.startswith("check-compile-fail: cache not used for ")
                and "donegate::verdict_compile_fail" in ln
                for ln in err.splitlines()
            ),
            err,
        )


class TestRecord(CacheBase):
    def test_c3_failed_target_writes_no_entry_and_reruns(self):
        d = "donegate::verdict_compile_fail"
        rc, err, ran = self.run_script(env={"CFC_FAIL": d})
        self.assertEqual(rc, 1, err)
        self.assertEqual(len(ran), 4, err)
        self.assertTrue(self.cache.exists(), "passing targets must be recorded")
        self.assertNotIn(d, self.entries())
        self.assertEqual(len(self.entries()), 3)
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertRanOnly(ran, [d], err)

    def test_c3_pass_then_fail_removes_entry_then_pass_skips_again(self):
        d = "donegate::verdict_compile_fail"
        self.prime()
        self.assertIn(d, self.entries())
        self.commit("crates/donegate/src/lib.rs", "// break\n", "a")
        rc, err, ran = self.run_script(env={"CFC_FAIL": d})
        self.assertEqual(rc, 1, err)
        self.assertEqual(ran, [d], err)
        self.assertNotIn(d, self.entries())
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertEqual(ran, [d], err)
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertEqual(ran, [], err)

    def test_c3_undetermined_target_writes_no_entry(self):
        d = "donegate::verdict_compile_fail"
        rc, err, ran = self.run_script(env={"CFC_ABNORMAL": d})
        self.assertEqual(rc, 2, err)
        if self.cache.exists():
            try:
                self.assertNotIn(d, self.entries())
            except (ValueError, KeyError):
                self.fail("cache written in an unreadable shape")
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertIn(d, ran)

    def test_c5_file_format(self):
        self.prime()
        data = json.loads(self.cache.read_text())
        self.assertEqual(data["version"], 1)
        self.assertEqual(set(data), {"version", "entries"})
        self.assertEqual(sorted(data["entries"]), sorted(LABELS))
        for v in data["entries"].values():
            self.assertRegex(v, r"^[0-9a-f]{12,}$")

    def test_c5_write_failure_warns_and_keeps_exit_code(self):
        if os.geteuid() == 0:
            self.skipTest("root ignores directory modes")
        ro = self.tmp / "ro"
        ro.mkdir()
        ro.chmod(0o555)
        rc, err, ran = self.run_script(cache=ro / "cache.json")
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4)
        self.assertIn("check-compile-fail: could not write cache: ", err)

    def test_c5_cache_path_is_directory_warns_and_keeps_exit_code(self):
        d = self.tmp / "cachedir"
        d.mkdir()
        rc, err, ran = self.run_script(cache=d)
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4)
        self.assertIn("check-compile-fail: could not write cache: ", err)
        self.assertEqual(self.skipped(err), {})

    def test_c5_write_failure_does_not_mask_failed_target(self):
        d = self.tmp / "cachedir"
        d.mkdir()
        rc, err, ran = self.run_script(
            cache=d, env={"CFC_FAIL": "donegate::verdict_compile_fail"}
        )
        self.assertEqual(rc, 1, err)

    def test_c6_no_cache_env_runs_all_and_leaves_file_alone(self):
        self.prime()
        before = self.cache.read_bytes()
        rc, err, ran = self.run_script(env={"CHECK_COMPILE_FAIL_NO_CACHE": "1"})
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4, err)
        self.assertEqual(self.skipped(err), {})
        self.assertEqual(self.cache.read_bytes(), before)

    def test_c6_no_cache_env_creates_no_file(self):
        rc, err, ran = self.run_script(env={"CHECK_COMPILE_FAIL_NO_CACHE": "1"})
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4)
        self.assertFalse(self.cache.exists())
        # control: the same fixture without the knob does create it
        self.run_script()
        self.assertTrue(self.cache.exists())

    def test_c9_trybuild_set_records_nothing_and_is_stripped(self):
        rc, err, ran = self.run_script(env={"TRYBUILD": "overwrite"})
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4)
        self.assertNotIn("TRYBUILD=1", self.last_log)
        if self.cache.exists():
            self.assertEqual(self.entries(), {})
        # control: without TRYBUILD the same fixture records
        self.run_script()
        self.assertEqual(len(self.entries()), 4)


class TestUndetermined(CacheBase):
    def _corrupt_and_run(self, content):
        self.prime()
        if isinstance(content, str):
            self.cache.write_text(content)
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4, err)
        self.assertEqual(self.skipped(err), {}, err)
        self.assertIn("cache not used", err)

    def test_c4_cache_not_json(self):
        self._corrupt_and_run("this is not json")

    def test_c4_cache_wrong_shape_list(self):
        self._corrupt_and_run("[1, 2, 3]")

    def test_c4_cache_wrong_shape_entries_not_object(self):
        self._corrupt_and_run('{"version": 1, "entries": ["x"]}')

    def test_c4_cache_wrong_shape_entry_value(self):
        self.prime()
        data = json.loads(self.cache.read_text())
        data["entries"] = {k: 12345 for k in data["entries"]}
        self.cache.write_text(json.dumps(data))
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4, err)
        self.assertEqual(self.skipped(err), {}, err)

    def test_c4_cache_path_is_directory(self):
        d = self.tmp / "cachedir"
        d.mkdir()
        rc, err, ran = self.run_script(cache=d)
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4)
        self.assertEqual(self.skipped(err), {})

    def test_c4_metadata_exit1(self):
        self.prime()
        rc, err, ran = self.run_script(env={"CFC_META": "exit1"})
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4, err)
        self.assertEqual(self.skipped(err), {}, err)
        self.assertIn("cache not used", err)

    def test_c4_metadata_garbage(self):
        self.prime()
        rc, err, ran = self.run_script(env={"CFC_META": "garbage"})
        self.assertEqual(rc, 0, err)
        self.assertEqual(len(ran), 4, err)
        self.assertEqual(self.skipped(err), {}, err)
        self.assertIn("cache not used", err)

    def test_c4_metadata_lacks_package(self):
        self.prime()
        rc, err, ran = self.run_script(env={"CFC_META": "missing"})
        self.assertEqual(rc, 0, err)
        self.assertIn("donegate::verdict_compile_fail", ran, err)
        self.assertNotIn("donegate::verdict_compile_fail", self.skipped(err))
        self.assertTrue(
            any(
                ln.startswith("check-compile-fail: cache not used for donegate::")
                for ln in err.splitlines()
            ),
            err,
        )

    def test_l5_registry_node_missing_for_closure_dep_runs_target(self):
        self.prime()
        rc, err, ran = self.run_script(env={"CFC_META": "regnode_missing"})
        self.assertEqual(rc, 0, err)
        self.assertIn("donegate::verdict_compile_fail", ran, err)
        self.assertNotIn("donegate::verdict_compile_fail", self.skipped(err))
        self.assertTrue(
            any(
                ln.startswith("check-compile-fail: cache not used for donegate::")
                for ln in err.splitlines()
            ),
            err,
        )

    def test_l5_registry_node_id_unreadable_runs_target(self):
        self.prime()
        rc, err, ran = self.run_script(env={"CFC_META": "regid_bad"})
        self.assertEqual(rc, 0, err)
        self.assertIn("donegate::verdict_compile_fail", ran, err)
        self.assertNotIn("donegate::verdict_compile_fail", self.skipped(err))
        self.assertTrue(
            any(
                ln.startswith("check-compile-fail: cache not used for donegate::")
                for ln in err.splitlines()
            ),
            err,
        )

    def test_c4_undetermined_cache_still_honours_cargo_exit_codes(self):
        self.prime()
        rc, err, ran = self.run_script(
            env={"CFC_META": "exit1", "CFC_FAIL": "donegate::verdict_compile_fail"}
        )
        self.assertEqual(rc, 1, err)
        self.assertEqual(len(ran), 4, err)
        self.assertEqual(self.skipped(err), {}, err)
        rc, err, ran = self.run_script(
            env={"CFC_META": "garbage", "CFC_ABNORMAL": "blastguard::verdict_compile_fail"}
        )
        self.assertEqual(rc, 2, err)
        self.assertEqual(self.skipped(err), {}, err)


class TestExitCodes(CacheBase):
    def test_c7_failed_target_exit_1(self):
        rc, err, _ = self.run_script(env={"CFC_FAIL": "blastguard::verdict_compile_fail"})
        self.assertEqual(rc, 1, err)
        self.assertIn("FAILED blastguard::verdict_compile_fail", err)

    def test_c7_abnormal_cargo_exit_2(self):
        rc, err, _ = self.run_script(
            env={"CFC_ABNORMAL": "harness-core::scope_compile_fail"}
        )
        self.assertEqual(rc, 2, err)
        self.assertIn("UNDETERMINED", err)

    def test_c7_failed_with_full_cache_around_it(self):
        self.prime()
        self.commit("crates/donegate/src/lib.rs", "// x\n", "a")
        rc, err, ran = self.run_script(env={"CFC_FAIL": "donegate::verdict_compile_fail"})
        self.assertEqual(rc, 1, err)
        self.assertEqual(ran, ["donegate::verdict_compile_fail"], err)
        self.assertEqual(len(self.skipped(err)), 3, err)

    def test_c8_wrong_channel_with_full_cache_is_undetermined(self):
        self.prime()
        rc, err, ran = self.run_script(env={"CFC_RUSTC_VERSION": "1.0.0"})
        self.assertEqual(rc, 2, err)
        self.assertEqual(ran, [], err)
        self.assertEqual(self.skipped(err), {}, err)
        # control: right channel on the same cache skips everything
        rc, err, ran = self.run_script()
        self.assertEqual(rc, 0, err)
        self.assertEqual(ran, [], err)


if __name__ == "__main__":
    unittest.main()
