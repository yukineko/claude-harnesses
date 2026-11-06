#!/usr/bin/env python3
"""INTENTIONALLY RED repro (filed 2026-11-07, session c2e3bc17):
scripts/test-changed-crates.sh reports an ENVIRONMENT failure — the test binary
deleted under a running `cargo test` by a concurrent `cargo clean` — as an
ordinary red test ("FAILED for <pkg>"), indistinguishable from a real failure.

Observed: on root ~/.condukt/worktrees/run-20261107-034825-70254-c2-8cd825f3,
donegate's test-changed failed with
`binary spawns: Os { code: 2, kind: NotFound, ... }` at
crates/backlog/tests/common/mod.rs:275 (CARGO_BIN_EXE_backlog), while the same
suite at main was green (backlog 672 passed / 0 failed). The cause there is
UNVERIFIED (a worker's concurrent cargo in the same target dir, or
scripts/cap-target-dir.sh's `cargo clean`).

What this file makes deterministic is one sufficient mechanism, measured
2026-11-07 with cargo 1.97.1: `cargo clean` does NOT wait for a `cargo test`
that is already executing its test binary; it deletes target/ underneath it and
the test fails with exactly that NotFound message. scripts/cap-target-dir.sh
runs `cargo clean` whenever the target dir is over CARGO_TARGET_CAP_MB, and
test-changed-crates.sh, lint-changed-crates.sh and other cargo runs on the same
root each invoke it — so two of them on one root can do this to each other.

Sequencing (no sleeps-as-synchronisation): the fixture's only test writes a
`ready` file, then waits for a `go` file before spawning CARGO_BIN_EXE. The
driver starts test-changed-crates.sh, waits for `ready`, runs cap-target-dir.sh
with CARGO_TARGET_CAP_MB=1 to completion (the target dir is several MB, so it
cleans), then writes `go`.

Property asserted (RED): the crate's test is green on its own (control), so a
non-zero exit here is not a test verdict. test-changed-crates.sh must either
exit 0 (e.g. the cap does not clean a target dir in use / the run is retried)
or say the failure is an environment / undetermined one — its output must not
be only the ordinary "FAILED for" red. Today it prints
"test-changed-crates: FAILED for scratch-race" and nothing else of its own.

Plain RED (no expectedFailure) because `backlog add --repro-test` requires a
behavioural failure at HEAD. Throwaway repo in a temp dir.

    python3 -m pytest -q scripts/test_test_changed_env_failure_20261107.py
"""

from __future__ import annotations

import os
import shutil
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
TEST = _HERE / "test-changed-crates.sh"
CAP = _HERE / "cap-target-dir.sh"

ROOT_MANIFEST = '[workspace]\nresolver = "2"\nmembers = ["crates/race"]\n'
RACE_MANIFEST = '[package]\nname = "scratch-race"\nversion = "0.0.0"\nedition = "2021"\n'
RACE_MAIN = 'fn main() {\n    println!("hi");\n}\n'
RACE_TEST = """\
#[test]
fn spawns_its_binary() {
    let d = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
    std::fs::write(d.join("ready"), "").unwrap();
    for _ in 0..1200 {
        if d.join("go").exists() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(100));
    }
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_scratch-race"))
        .output()
        .expect("binary spawns");
    assert!(out.status.success());
}
"""

ENV_MARKERS = ("environment", "undetermined")


class Repo:
    def __init__(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="test-changed-env-")).resolve()
        self.env = dict(os.environ)
        for k in ("GIT_DIR", "GIT_WORK_TREE", "GIT_INDEX_FILE", "CARGO_TARGET_DIR",
                  "CARGO_TARGET_CAP_MB"):
            self.env.pop(k, None)
        self.env.update(
            LC_ALL="C", GIT_CONFIG_NOSYSTEM="1", GIT_CONFIG_GLOBAL=os.devnull,
            GIT_TERMINAL_PROMPT="0", GIT_CEILING_DIRECTORIES=str(self.root.parent),
            GIT_AUTHOR_NAME="t", GIT_AUTHOR_EMAIL="t@example.invalid",
            GIT_COMMITTER_NAME="t", GIT_COMMITTER_EMAIL="t@example.invalid",
        )
        self.git("init", "-q", "-b", "main")
        self.git("config", "commit.gpgsign", "false")
        self.write(".gitignore", "/target\n/crates/race/ready\n/crates/race/go\n")
        self.write("Cargo.toml", ROOT_MANIFEST)
        self.write("crates/race/Cargo.toml", RACE_MANIFEST)
        self.write("crates/race/src/main.rs", RACE_MAIN)
        sd = self.root / "scripts"
        sd.mkdir()
        for s in (TEST, CAP):
            shutil.copy2(s, sd / s.name)
            (sd / s.name).chmod(0o755)
        self.git("add", "-A")
        self.git("commit", "-q", "-m", "seed")
        # The touched crate: an uncommitted test, so test-changed selects it.
        self.write("crates/race/tests/spawn.rs", RACE_TEST)
        self.crate = self.root / "crates" / "race"

    def write(self, rel: str, text: str) -> None:
        p = self.root / rel
        p.parent.mkdir(parents=True, exist_ok=True)
        p.write_text(text)

    def git(self, *args: str) -> None:
        subprocess.run(["git", *args], cwd=str(self.root), env=self.env, check=True,
                       capture_output=True)

    def start_test_changed(self) -> subprocess.Popen:
        return subprocess.Popen([str(self.root / "scripts" / TEST.name)], cwd=str(self.root),
                                env=self.env, stdout=subprocess.PIPE, stderr=subprocess.PIPE,
                                text=True)

    def wait_ready(self, proc: subprocess.Popen) -> None:
        deadline = time.monotonic() + 600
        while not (self.crate / "ready").exists():
            if proc.poll() is not None:
                raise AssertionError("apparatus: test-changed exited before the test ran: "
                                     + (proc.stdout.read() + proc.stderr.read())[-1500:])
            if time.monotonic() > deadline:
                proc.kill()
                raise AssertionError("apparatus: the fixture test never started")
            time.sleep(0.1)

    def release(self) -> None:
        (self.crate / "go").write_text("")

    def cleanup(self) -> None:
        shutil.rmtree(self.root, ignore_errors=True)


@unittest.skipUnless(shutil.which("cargo"), "cargo not on PATH")
class TestChangedEnvironmentFailure(unittest.TestCase):
    def setUp(self) -> None:
        self.r = Repo()
        self.addCleanup(self.r.cleanup)

    def test_control_crate_is_green_without_interference(self) -> None:
        proc = self.r.start_test_changed()
        self.r.wait_ready(proc)
        self.r.release()
        out, err = proc.communicate(timeout=600)
        self.assertEqual(proc.returncode, 0, (out + err)[-1500:])

    def test_target_dir_cleaned_under_running_test_is_not_an_ordinary_red(self) -> None:
        proc = self.r.start_test_changed()
        self.r.wait_ready(proc)
        env = dict(self.r.env, CARGO_TARGET_CAP_MB="1")
        cap = subprocess.run([str(self.r.root / "scripts" / CAP.name)], cwd=str(self.r.root),
                             env=env, capture_output=True, text=True, timeout=300)
        self.assertEqual(cap.returncode, 0, cap.stderr)
        self.assertIn("running cargo clean", cap.stderr,
                      "apparatus: the concurrent cap must actually clean: " + cap.stderr)
        self.r.release()
        out, err = proc.communicate(timeout=600)
        combined = out + err
        self.assertIn("NotFound", combined,
                      "apparatus: the test binary must have been deleted under the run:\n"
                      + combined[-1500:])
        own_lines = "\n".join(l for l in combined.splitlines()
                              if l.startswith("test-changed-crates:"))
        self.assertTrue(
            proc.returncode == 0 or any(m in own_lines.lower() for m in ENV_MARKERS),
            "test-changed-crates.sh reported a deleted test binary (environment failure, the "
            "crate is green on its own) as an ordinary red (exit %d). Its own lines:\n%s"
            % (proc.returncode, own_lines),
        )


if __name__ == "__main__":
    unittest.main()
