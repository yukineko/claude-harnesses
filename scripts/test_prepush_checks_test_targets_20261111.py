#!/usr/bin/env python3
"""RED: .githooks/pre-push's typecheck stage lets a test-target compile error through.

Property (backlog 38a44b30 / GitHub #1641): pre-push blocks a push whose range
contains a crate whose OWN test targets do not compile, even when Cargo's
feature unification across the other packages the hook selects would hide it.

Observed by scripts/gate-severity-probe.py on 2026-11-11 (cf954b43): with the
`fault-injection-adoption` fixture (crates/tdd drops `features =
["fault-injection"]` from its harness-core dev-dependency) pre-push passed,
while `cargo test -p tdd` failed with E0432 (unresolved import
`harness_core::degrade::assert_fails_closed`, gated behind that feature).
Mechanism: when the selected packages include another member whose
harness-core dev-dependency enables `fault-injection`, Cargo unifies that
feature into tdd's test targets too, so the missing feature in tdd's own
manifest is invisible. `cargo check -p tdd` alone does not unify and fails.

RE-ANCHORED (2026-11-11). The previous version of this file regex-extracted the
`cargo check` line from the hook and ran `cargo check --all-targets --workspace`
itself. That only observed Cargo's feature unification, never the hook, so no
change to the hook could turn it green. It was replaced because of the user's
standing rule that a test must observe the named property: this version runs
the real hook (`sh .githooks/pre-push <remote> <url>` with git's stdin ref
lines) and keys on what the hook reports for its typecheck stage.

How the hook is driven:
  * a throwaway `git clone --shared` of HEAD in the system temp dir (removed in
    tearDown); CARGO_TARGET_DIR defaults to this checkout's target/ for cache.
  * a base commit and a fixture commit are made in the clone; the fixture is
    checked out clean, and the hook gets one stdin line
    `refs/heads/fixture <fixture> refs/heads/main <base>`, so its range is
    exactly base..fixture.
  * the OTHER blocking stages (compile-fail, specguard gate-check, fail-open
    mutation) are pre-recorded as passed for the fixture tree in a temp
    TEST_PASS_RECORD_DIR, so they skip and the run stays bounded. The typecheck
    stage's id is never pre-recorded.
  * the verdict is read from the hook's own per-stage report: the last JSON
    line of `<clone git dir>/prepush-timing.jsonl` must name `typecheck` (for
    the fixture sha) as the phase the hook stopped in, with a nonzero exit, and
    stderr must carry the tdd compile error (`assert_fails_closed`), so a block
    for an unrelated reason in that stage does not count.

Scenarios:
  W  the fixture commit drops the feature AND micro-bumps tdd (Cargo.toml +
     Cargo.lock), as every plugin commit does. Cargo.lock in the range widens
     the hook's check to the whole workspace.
  N  the narrow path. A version bump always touches Cargo.lock, which widens,
     so N cannot carry the bump inside the range: the bumps of tdd and
     blastguard sit in the base commit (already on the remote), and the range
     touches only crates/tdd (the feature drop) and crates/blastguard (a
     comment), whose harness-core dev-dependency has `fault-injection`.

What this does NOT prove: anything about the stages it pre-records (they are
skipped on purpose), the hook copy in the clone's scripts/ (the hook file run
is this checkout's .githooks/pre-push, but the scripts it calls are the
clone's, i.e. HEAD's), or the real push transport.

Control (both scenarios): `cargo check --all-targets -p tdd` exits nonzero on
the fixture tree, so the injection really breaks tdd's test target.

GREEN since pre-push checks each touched crate alone (`cargo check
--all-targets -p <crate>`) after the combined check; the strict xfail markers
were removed then.

Run:  python3 -m pytest scripts/test_prepush_checks_test_targets_20261111.py
"""

from __future__ import annotations

import json
import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
HOOK = REPO / ".githooks" / "pre-push"

FEATURE_DEP = 'harness-core = { path = "../harness-core", features = ["fault-injection"] }'
PLAIN_DEP = 'harness-core = { path = "../harness-core" }'
GATE_CRATES = ("blastguard", "propguard", "specguard", "stuckguard", "mutategate", "overwatch", "parallelguard")
GIT_ID = ["-c", "user.name=prepush-test", "-c", "user.email=prepush-test@example.invalid",
          "-c", "commit.gpgsign=false", "-c", "core.hooksPath=/dev/null"]


def micro_bump(v: str) -> str:
    major, minor, patch = v.split(".")
    return f"{major}.{minor}.{int(patch) + 1}"


class PrePushTypecheckBlocksTestTargetBreak(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="prepush-test-targets-", dir=tempfile.gettempdir()))
        self.clone = self.tmp / "clone"
        self.records = self.tmp / "pass-records"
        self.records.mkdir()
        head = self._run(["git", "-C", str(REPO), "rev-parse", "HEAD"], cwd=REPO).stdout.strip()
        self._run(["git", "clone", "-q", "--shared", str(REPO), str(self.clone)], cwd=self.tmp)
        self._git("checkout", "-q", "--detach", head)
        self.env = dict(os.environ)
        cargo_bin = Path.home() / ".cargo" / "bin"
        self.env["PATH"] = f"{cargo_bin}{os.pathsep}{self.env.get('PATH', '')}"
        self.env.setdefault("CARGO_TARGET_DIR", str(REPO / "target"))
        self.env["TEST_PASS_RECORD_DIR"] = str(self.records)

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    # --- helpers -----------------------------------------------------------
    def _run(self, argv, cwd, check=True, **kw):
        proc = subprocess.run(argv, cwd=cwd, capture_output=True, text=True, **kw)
        if check and proc.returncode != 0:
            raise AssertionError(f"{argv} exited {proc.returncode}:\n{proc.stderr[-2000:]}")
        return proc

    def _git(self, *args, check=True):
        return self._run(["git", *GIT_ID, *args], cwd=self.clone, check=check)

    def _edit(self, rel: str, old: str, new: str) -> None:
        p = self.clone / rel
        text = p.read_text(encoding="utf-8")
        self.assertIn(old, text, f"{rel}: anchor not found")
        p.write_text(text.replace(old, new, 1), encoding="utf-8")

    def _bump(self, crate: str) -> None:
        """Micro bump in crates/<crate>/Cargo.toml and Cargo.lock (touches Cargo.lock)."""
        rel = f"crates/{crate}/Cargo.toml"
        old = re.search(r'^version = "([^"]+)"', (self.clone / rel).read_text(encoding="utf-8"), re.M).group(1)
        new = micro_bump(old)
        self._edit(rel, f'version = "{old}"', f'version = "{new}"')
        self._edit("Cargo.lock", f'name = "{crate}"\nversion = "{old}"', f'name = "{crate}"\nversion = "{new}"')

    def _drop_feature(self) -> None:
        # Test-target-only compile error: tdd's tests/fault_injection.rs imports
        # a harness-core item that exists only with the `fault-injection` feature.
        self._edit("crates/tdd/Cargo.toml", FEATURE_DEP, PLAIN_DEP)

    def _commit(self, msg: str) -> str:
        self._git("add", "-A")
        self._git("commit", "-q", "-m", msg)
        return self._git("rev-parse", "HEAD").stdout.strip()

    def _range_files(self, base: str, sha: str) -> list[str]:
        return self._git("diff", "--name-only", base, sha).stdout.split()

    def _seed_other_stages(self, sha: str, base: str) -> None:
        """Pre-record passes for every blocking stage except typecheck."""
        tree = self._git("rev-parse", f"{sha}^{{tree}}").stdout.strip()
        ids = ["compile-fail", f"specguard-gate-check:{base}"] + [f"fail-open-mutation:{g}" for g in GATE_CRATES]
        for test_id in ids:
            self._run(["python3", str(self.clone / "scripts" / "test-pass-record.py"),
                       "record", "--tree", tree, "--test", test_id],
                      cwd=self.clone, env=self.env)
            self.assertFalse(test_id.startswith("cargo-check"))

    def _run_hook(self, sha: str, base: str) -> subprocess.CompletedProcess:
        self._git("branch", "-f", "fixture", sha)
        self._git("checkout", "-q", "fixture")
        self.assertEqual(self._git("status", "--porcelain").stdout, "", "fixture checkout is not clean")
        self._seed_other_stages(sha, base)
        stdin = f"refs/heads/fixture {sha} refs/heads/main {base}\n"
        return subprocess.run(
            ["sh", str(HOOK), "origin", "https://example.invalid/harness.git"],
            cwd=self.clone, env=self.env, input=stdin, capture_output=True, text=True, timeout=1800,
        )

    def _last_timing(self) -> dict:
        common = self._git("rev-parse", "--git-common-dir").stdout.strip()
        log = (self.clone / common).resolve() / "prepush-timing.jsonl"
        self.assertTrue(log.exists(), f"hook wrote no timing log at {log}")
        lines = [ln for ln in log.read_text(encoding="utf-8").splitlines() if ln.strip()]
        self.assertTrue(lines, "timing log is empty")
        return json.loads(lines[-1])

    def _control(self) -> subprocess.CompletedProcess:
        return self._run(["cargo", "check", "--all-targets", "--quiet", "-p", "tdd"],
                         cwd=self.clone, check=False, env=self.env, timeout=1800)

    def _assert_typecheck_blocks(self, sha: str, base: str) -> None:
        res = self._run_hook(sha, base)
        timing = self._last_timing()
        phases = [(p.get("phase"), p.get("sha")) for p in timing.get("phases", [])]
        tail = res.stderr[-4000:]

        # Control: the per-package check sees the error (the injection bites).
        ctl = self._control()
        self.assertNotEqual(ctl.returncode, 0,
                            "injection did not break tdd's test target:\n" + ctl.stderr[-2000:])

        self.assertIn(("typecheck", sha), phases,
                      f"the hook never reached its typecheck stage for {sha}: phases={phases}\n{tail}")
        self.assertEqual(
            phases[-1], ("typecheck", sha),
            f"pre-push did not stop in its typecheck stage for {sha[:12]} although "
            f"`cargo check --all-targets -p tdd` exits {ctl.returncode}; "
            f"stages reached={phases}, hook exit={res.returncode}, "
            f"timing exit={timing.get('exit')}\n--- hook stderr (tail) ---\n{tail}",
        )
        self.assertNotEqual(timing.get("exit"), 0, f"typecheck was the last stage but exit 0\n{tail}")
        self.assertNotEqual(res.returncode, 0, tail)
        self.assertIn("assert_fails_closed", res.stderr,
                      f"the typecheck block is not tdd's test-target compile error\n{tail}")

    # --- scenarios ----------------------------------------------------------
    def test_W_cargo_lock_range_typecheck_blocks_tdd_test_target_break(self):
        base = self._git("rev-parse", "HEAD").stdout.strip()
        self._drop_feature()
        self._bump("tdd")
        sha = self._commit("fixture W: tdd drops fault-injection dev feature + lockstep bump")
        files = self._range_files(base, sha)
        self.assertIn("Cargo.lock", files, "scenario W must widen via Cargo.lock")
        self._assert_typecheck_blocks(sha, base)

    def test_N_narrow_range_typecheck_blocks_tdd_test_target_break(self):
        # The bumps (and so the Cargo.lock change) sit in the base: a range that
        # contains any version bump touches Cargo.lock and widens.
        self._bump("tdd")
        self._bump("blastguard")
        base = self._commit("base N: lockstep bumps of tdd and blastguard")
        self._drop_feature()
        lib = self.clone / "crates" / "blastguard" / "src" / "lib.rs"
        lib.write_text(lib.read_text(encoding="utf-8") + "// prepush fixture N touch\n", encoding="utf-8")
        sha = self._commit("fixture N: tdd drops fault-injection dev feature; blastguard touched")
        files = self._range_files(base, sha)
        self.assertEqual(sorted(files), ["crates/blastguard/src/lib.rs", "crates/tdd/Cargo.toml"],
                         "scenario N must touch only tdd and blastguard (no root cargo file)")
        self.assertIn(FEATURE_DEP, (self.clone / "crates/blastguard/Cargo.toml").read_text(encoding="utf-8"),
                      "blastguard's harness-core dev-dependency no longer enables fault-injection")
        self._assert_typecheck_blocks(sha, base)


if __name__ == "__main__":
    unittest.main()
