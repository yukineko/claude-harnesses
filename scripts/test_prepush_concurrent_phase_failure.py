#!/usr/bin/env python3
"""Pre-push blocking phases (typecheck / compile-fail / specguard gate-check)
must BLOCK when any one of them fails, no matter how the phases are scheduled
(sequentially today, possibly concurrently after the planned change).

Run: `python3 scripts/test_prepush_concurrent_phase_failure.py` (stdlib unittest).
Reuses HookTestCase/HookHarness from scripts/test_prepush_hook.py. Every
external (cargo, scripts/check-compile-fail.py, target/debug/specguard,
scripts/check-fail-open-mutation.py) is a stub controlled through env vars in
`h.env`; nothing real is compiled.

Phases and their pass-record test ids for a push touching only
crates/blastguard (a gate crate, so all three run):
  typecheck    -> `cargo-check:crates/blastguard`   (cargo check ... stub)
  compile-fail -> `compile-fail`                    (check-compile-fail.py stub)
  specguard    -> `specguard-gate-check:<base sha>` (specguard gate-check stub)

Stub knobs (env, per phase P in CHECK / CF / SG):
  P_EXIT=<n>   exit code           P_SLEEP=<secs> sleep before finishing
  P_KILL=1     SIGKILL itself instead of exiting

Tests: c1 each single failing phase blocks and leaves no pass record for it,
c2 a signal-killed phase blocks, c3 slow-pass + fast-fail still blocks,
c4 control: all pass => exit 0 and all three records exist.

DOES NOT PROVE: that phases run concurrently (see
test_prepush_phases_overlap.py), that a failing phase's output is shown, or
anything about records of OTHER phases when one fails (c3 deliberately leaves
the slow phase's record unasserted). A hook that blocked every push would pass
c1-c3 trivially; c4 is the anti-vacuity control.
"""

from __future__ import annotations

import json
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_prepush_hook import HookTestCase  # noqa: E402

PY_PHASE_STUB = """#!/usr/bin/env python3
import os, signal, sys, time
P = {prefix!r}
log = os.environ.get("PHASE_LOG")
def note(what):
    if log:
        with open(log, "a") as f:
            f.write("%s %s %.6f\\n" % (P, what, time.time()))
note("start")
s = float(os.environ.get(P + "_SLEEP", "0"))
if s:
    time.sleep(s)
note("end")
if os.environ.get(P + "_KILL") == "1":
    os.kill(os.getpid(), signal.SIGKILL)
sys.exit(int(os.environ.get(P + "_EXIT", "0")))
"""

# cargo: `metadata` answers with the JSON the hook reads; `check` is phase
# CHECK; every other subcommand (build) succeeds immediately.
CARGO_STUB = """#!/usr/bin/env python3
import os, signal, sys, time
if sys.argv[1:2] == ["metadata"]:
    sys.stdout.write(os.environ["CARGO_META_JSON"])
    sys.exit(0)
if sys.argv[1:2] != ["check"]:
    sys.exit(0)
P = "CHECK"
log = os.environ.get("PHASE_LOG")
def note(what):
    if log:
        with open(log, "a") as f:
            f.write("%s %s %.6f\\n" % (P, what, time.time()))
note("start")
s = float(os.environ.get(P + "_SLEEP", "0"))
if s:
    time.sleep(s)
note("end")
if os.environ.get(P + "_KILL") == "1":
    os.kill(os.getpid(), signal.SIGKILL)
sys.exit(int(os.environ.get(P + "_EXIT", "0")))
"""

# specguard binary: `map build` succeeds; `map gate-check` is phase SG.
SPECGUARD_STUB = """#!/usr/bin/env python3
import os, signal, sys, time
if "gate-check" not in sys.argv[1:]:
    sys.exit(0)
P = "SG"
log = os.environ.get("PHASE_LOG")
def note(what):
    if log:
        with open(log, "a") as f:
            f.write("%s %s %.6f\\n" % (P, what, time.time()))
note("start")
s = float(os.environ.get(P + "_SLEEP", "0"))
if s:
    time.sleep(s)
note("end")
if os.environ.get(P + "_KILL") == "1":
    os.kill(os.getpid(), signal.SIGKILL)
sys.exit(int(os.environ.get(P + "_EXIT", "0")))
"""

MUT_STUB = "import sys\nsys.exit(0)\n"
GATE_PKG = '[package]\nname = "%s"\nversion = "0.1.0"\nedition = "2021"\n'
CHECK_ID = "cargo-check:crates/blastguard"
CF_ID = "compile-fail"


def _out(proc):
    return "--- exit %d ---\n--- stdout ---\n%s\n--- stderr ---\n%s" % (
        proc.returncode, proc.stdout, proc.stderr)


class PhaseTestCase(HookTestCase):
    """Builds a harness whose three blocking phases are all controllable stubs."""

    def phase_harness(self, mut_stub=None, **env):
        h = self.harness(crate=True)
        h.phase_log = h.logdir / "phases.log"
        # `sleep` is not among the tools the harness links onto the hook's PATH.
        import shutil
        import os
        sl = shutil.which("sleep")
        if sl and not (h.bindir / "sleep").exists():
            os.symlink(sl, h.bindir / "sleep")
        pkgs = [{"name": n, "version": "0.1.0", "id": n + " 0.1.0",
                 "manifest_path": str(h.root / "crates" / n / "Cargo.toml")}
                for n in ("foo", "blastguard")]
        h.env["CARGO_META_JSON"] = json.dumps({
            "packages": pkgs, "workspace_members": [p["id"] for p in pkgs],
            "target_directory": str(h.root / "target"),
            "workspace_root": str(h.root), "version": 1})
        h.env["PHASE_LOG"] = str(h.phase_log)
        cargo = h.bindir / "cargo"
        cargo.write_text(CARGO_STUB)
        cargo.chmod(0o755)
        sg = h.root / "target" / "debug" / "specguard"
        sg.parent.mkdir(parents=True)
        sg.write_text(SPECGUARD_STUB)
        sg.chmod(0o755)
        h.commit_files({
            "scripts/check-compile-fail.py": PY_PHASE_STUB.format(prefix="CF"),
            "scripts/check-fail-open-mutation.py": mut_stub or MUT_STUB,
        }, "phase stubs")
        h.base = h.local_sha
        h.env.update(env)
        h.commit_files({
            "crates/blastguard/Cargo.toml": GATE_PKG % "blastguard",
            "crates/blastguard/src/lib.rs": "pub fn g() {}\n",
        }, "touch gate crate")
        h.tree = h.tree_of(h.local_sha)
        h.sg_id = "specguard-gate-check:" + h.base
        return h

    def push(self, h):
        return h.run(stdin="refs/heads/main %s refs/heads/main %s\n" % (h.local_sha, h.base))

    def ids(self, h):
        return {"typecheck": CHECK_ID, "compile-fail": CF_ID, "specguard": h.sg_id}

    def phase_log_lines(self, h):
        if not h.phase_log.is_file():
            return []
        return h.phase_log.read_text().splitlines()


KNOB = {"typecheck": "CHECK", "compile-fail": "CF", "specguard": "SG"}


class SingleFailingPhaseBlocks(PhaseTestCase):
    def test_c1_each_single_failing_phase_blocks_and_is_not_recorded(self):
        """c1. Exactly one phase fails (exit 1), the other two pass.
        Does NOT prove the other phases' records either way."""
        for phase, knob in KNOB.items():
            with self.subTest(phase):
                h = self.phase_harness(**{knob + "_EXIT": "1"})
                proc = self.push(h)
                self.assertBlocked(proc, _out(proc))
                self.assertFalse(h.record_path(h.tree, self.ids(h)[phase]).exists(),
                                 "failed phase %s must not leave a pass record\n%s" % (phase, _out(proc)))
                # The failing stub actually ran (not blocked for another reason).
                self.assertIn("%s start" % knob, " ".join(
                    ln.rsplit(" ", 1)[0] for ln in self.phase_log_lines(h)), _out(proc))

    def test_c2_signal_killed_phase_blocks(self):
        """c2. A phase process dies from SIGKILL: no exit code => undetermined => block.
        Does NOT prove which message is printed."""
        for phase, knob in KNOB.items():
            with self.subTest(phase):
                h = self.phase_harness(**{knob + "_KILL": "1"})
                proc = self.push(h)
                self.assertBlocked(proc, _out(proc))
                self.assertFalse(h.record_path(h.tree, self.ids(h)[phase]).exists(), _out(proc))

    def test_c3_slow_pass_plus_fast_fail_still_blocks(self):
        """c3. One phase is slow and passes (2s), another is fast and fails.
        Whatever the scheduling, the push is blocked and the failed phase has no
        record. Does NOT assert whether the slow phase's pass is recorded."""
        phases = list(KNOB.items())
        for fail_phase, fail_knob in phases:
            for slow_phase, slow_knob in phases:
                if slow_phase == fail_phase:
                    continue
                with self.subTest(fail=fail_phase, slow=slow_phase):
                    h = self.phase_harness(**{fail_knob + "_EXIT": "1", slow_knob + "_SLEEP": "2"})
                    proc = self.push(h)
                    self.assertBlocked(proc, _out(proc))
                    self.assertFalse(h.record_path(h.tree, self.ids(h)[fail_phase]).exists(), _out(proc))

    def test_c4_control_all_pass_allows_and_records_all(self):
        """c4. Anti-vacuity control: all three phases pass => exit 0 and each
        phase's pass record exists. Does NOT prove the phases ran in parallel."""
        h = self.phase_harness()
        proc = self.push(h)
        self.assertAllowed(proc, _out(proc))
        for phase, test_id in self.ids(h).items():
            self.assertTrue(h.record_path(h.tree, test_id).is_file(),
                            "no pass record for %s (%s)\n%s" % (phase, test_id, _out(proc)))


if __name__ == "__main__":
    unittest.main()
