#!/usr/bin/env python3
"""Audit B item 8 (memory: liveness-and-observability, section "A killed
background task can leave an orphan loop"; formerly
killed-task-leaves-orphan-loop).

Measured 2026-08-06: an orphaned verification loop kept running
`cargo test --workspace` beside a fresh one; both appended to the same logs and
several crates' tests share FIXED /tmp fixture paths, so a measurement taken
then is void. The memory's rule ("`ps ... | grep "[c]argo test"` and confirm
zero before measuring") is applied by recall only. Planned mechanization: the
measurement scripts refuse to start while a live `cargo test` exists.

Property asserted, with a real dummy process whose command line is
`<tmp>/cargo test --workspace` (a sleeping /bin/sh script; no real cargo runs):
  * scripts/check-workspace-tests.py `main()` (driven with the repo's own
    FakeRunner injection, so nothing real is launched either way) does NOT
    launch its cargo body and exits non-zero;
  * scripts/mutation-gate.sh, copied into a fixture repo with a logging fake
    `cargo` first on PATH, never starts `cargo mutants --package ...` and
    exits 2 (its existing "undetermined" code).

Exit code note: in check-workspace-tests.py, 2 is already PYTHON_FAILURE, so
this test only requires "non-zero and cargo not launched" there; which class
(5 ENVIRONMENT, or a new one) is the fix's call.

Controls: the dummy is visible to `ps -ax -o command` as "cargo test"; without
the dummy, the mutation-gate fixture does reach `cargo mutants --package`
(so the "never started" assertion is not vacuous). Caveat: the control would
also observe any REAL `cargo test` another session runs on this machine.
"""
import os
import shutil
import signal
import subprocess
import tempfile
import time
import unittest
from pathlib import Path

from scripts import test_check_workspace_tests as T

REPO = Path(__file__).resolve().parent.parent
cws = T.cws


class _LiveCargoTest:
    """A process whose ps command line reads `<tmp>/cargo test --workspace`."""

    def __init__(self, tmp):
        d = Path(tmp) / "dummy"
        d.mkdir()
        exe = d / "cargo"
        exe.write_text("#!/bin/sh\nwhile :; do sleep 1; done\n", encoding="utf-8")
        exe.chmod(0o755)
        self.proc = subprocess.Popen(
            [str(exe), "test", "--workspace"],
            stdin=subprocess.DEVNULL,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
            start_new_session=True,
        )
        time.sleep(0.3)

    def stop(self):
        try:
            os.killpg(self.proc.pid, signal.SIGKILL)
        except ProcessLookupError:
            pass
        self.proc.wait(timeout=10)


def _ps_sees_cargo_test(pid):
    out = subprocess.run(
        ["ps", "-ax", "-o", "pid=,command="], capture_output=True, text=True, check=True
    ).stdout
    return any(
        ln.split(None, 1)[0] == str(pid) and "cargo test" in ln for ln in out.splitlines()
    )


def _mutation_gate_fixture(tmp):
    root = Path(tmp) / "repo"
    (root / "scripts").mkdir(parents=True)
    shutil.copy2(REPO / "scripts" / "mutation-gate.sh", root / "scripts" / "mutation-gate.sh")
    fakebin = Path(tmp) / "fakebin"
    fakebin.mkdir()
    log = Path(tmp) / "cargo.log"
    cargo = fakebin / "cargo"
    cargo.write_text(
        "#!/bin/sh\n"
        f'echo "$*" >> "{log}"\n'
        'if [ "$1 $2" = "mutants --version" ]; then echo "cargo-mutants 0.0.0"; exit 0; fi\n'
        "exit 70\n",
        encoding="utf-8",
    )
    cargo.chmod(0o755)
    home = Path(tmp) / "home"
    home.mkdir()
    env = {
        "PATH": f"{fakebin}:/usr/bin:/bin",
        "HOME": str(home),
        "PILOT": "audit-b-fixture",
    }
    return root, env, log


def _run_mutation_gate(root, env):
    return subprocess.run(
        ["bash", str(root / "scripts" / "mutation-gate.sh")],
        cwd=root,
        env=env,
        stdout=subprocess.PIPE,
        stderr=subprocess.STDOUT,
        text=True,
        timeout=60,
    )


def _mutants_runs(log):
    if not log.exists():
        return []
    return [ln for ln in log.read_text().splitlines() if ln.startswith("mutants --package")]


class RefuseWhileCargoTestIsLive(unittest.TestCase):
    def setUp(self):
        self._tmp = tempfile.TemporaryDirectory()
        self.tmp = self._tmp.name

    def tearDown(self):
        self._tmp.cleanup()

    def _with_live(self):
        live = _LiveCargoTest(self.tmp)
        self.addCleanup(live.stop)
        return live

    def test_control_dummy_is_visible_as_cargo_test(self):
        live = self._with_live()
        self.assertTrue(_ps_sees_cargo_test(live.proc.pid))

    def test_control_mutation_gate_reaches_cargo_mutants_without_dummy(self):
        root, env, log = _mutation_gate_fixture(self.tmp)
        p = _run_mutation_gate(root, env)
        self.assertTrue(_mutants_runs(log), p.stdout)

    def test_check_workspace_tests_refuses_while_cargo_test_is_live(self):
        self._with_live()
        repo = T._fixture_repo(os.path.join(self.tmp, "ws"))
        runner = T.FakeRunner()
        rc, out, err = T.MainCase().run_main(repo, runner)
        self.assertEqual(
            runner.cargo_calls,
            [],
            "check-workspace-tests launched `cargo test --workspace` while another "
            f"`cargo test` was live (rc={rc}); the measurement is void",
        )
        self.assertNotEqual(rc, cws.RC_OK, f"out={out}\nerr={err}")

    def test_mutation_gate_refuses_while_cargo_test_is_live(self):
        self._with_live()
        root, env, log = _mutation_gate_fixture(self.tmp)
        p = _run_mutation_gate(root, env)
        self.assertEqual(
            _mutants_runs(log),
            [],
            "mutation-gate.sh started `cargo mutants` while another `cargo test` "
            f"was live (rc={p.returncode}): {p.stdout[-300:]}",
        )
        self.assertEqual(p.returncode, 2, p.stdout)


if __name__ == "__main__":
    unittest.main()
