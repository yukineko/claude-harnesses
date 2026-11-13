#!/usr/bin/env python3
"""Hook tests for pre-push blocking check 5 (fail-open mutation) relative to
the in-place chain (typecheck -> compile-fail -> specguard) for the same
pushed sha.

HISTORY: written (by a non-implementer agent) for a design in which the
mutation ran concurrently with the chain in a temporary checkout. The user
revoked that on 2026-11-14 (「gateにそんな例外は追加してほしくない」): the
mutation stays in place and serial, after the chain. m1 was therefore inverted
from "overlaps" to "never overlaps" (the mutation rewrites source in this
worktree, so overlapping it with a cargo build of the same tree would judge a
mutant). m2-m5 are unchanged.

Run: `python3 scripts/test_prepush_mutation_concurrent.py` (stdlib unittest).
Reuses PhaseTestCase from test_prepush_concurrent_phase_failure.py (stub cargo,
compile-fail script, specguard binary). The mutation script is a committed stub
driven by env vars:
  MUT_EXIT=<n>  MUT_KILL=1 (SIGKILL itself)  MUT_SLEEP=<s>
  MUT_PIDFILE / MUT_MARKER: files the stub writes its pid / "done" to.
  Start/end timestamps go to $PHASE_LOG as `MUT start|end <t>`.
The push touches only crates/blastguard, so the stub is invoked as
`--crate blastguard --keep-going`; its record id is
`fail-open-mutation:blastguard`.

  m1  the mutation starts only after every in-place phase ended. (GUARD)
  m2  mutation exits 1, in-place phases pass => blocked, no record.
  m3  mutation SIGKILLs itself => blocked, no record.
  m4  typecheck fails fast while mutation sleeps 3s => blocked, and after the
      hook returns no mutation stub process is still alive.
  m5  control: all pass => exit 0 and the mutation record exists.

m2/m3/m5 are expected GUARDs at origin/main (sequential hook already behaves
so); m4's process-liveness part is trivially true sequentially (the mutation
never starts), so it is a GUARD there too.

DOES NOT PROVE: m4 does not assert whether the mutation stub started, nor
whether its result was recorded; nothing here runs the real
mutation script or real cargo. m4's pid check cannot see a descendant that
re-parented under a different pid than the one the stub wrote.
"""

from __future__ import annotations

import os
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

from test_prepush_concurrent_phase_failure import PhaseTestCase, _out  # noqa: E402

MUT_ID = "fail-open-mutation:blastguard"

TIMED_MUT_STUB = """import os, signal, sys, time
log = os.environ.get("PHASE_LOG")
def note(what):
    if log:
        with open(log, "a") as f:
            f.write("MUT %s %.6f\\n" % (what, time.time()))
pf = os.environ.get("MUT_PIDFILE")
if pf:
    with open(pf, "w") as f:
        f.write(str(os.getpid()))
note("start")
s = float(os.environ.get("MUT_SLEEP", "0"))
if s:
    time.sleep(s)
mk = os.environ.get("MUT_MARKER")
if mk:
    with open(mk, "w") as f:
        f.write("done")
note("end")
if os.environ.get("MUT_KILL") == "1":
    os.kill(os.getpid(), signal.SIGKILL)
sys.exit(int(os.environ.get("MUT_EXIT", "0")))
"""


class MutationConcurrent(PhaseTestCase):
    def mut_harness(self, **env):
        h = self.phase_harness(mut_stub=TIMED_MUT_STUB, **env)
        h.pidfile = h.logdir / "mut.pid"
        h.marker = h.logdir / "mut.marker"
        h.env["MUT_PIDFILE"] = str(h.pidfile)
        h.env["MUT_MARKER"] = str(h.marker)
        return h

    def intervals(self, h):
        t = {}
        for ln in self.phase_log_lines(h):
            name, what, ts = ln.split()
            t.setdefault(name, {})[what] = float(ts)
        return t

    def test_m1_mutation_never_overlaps_the_in_place_phases(self):
        """m1 (inverted 2026-11-14, see the module docstring). The mutation
        rewrites source in THIS worktree, so it must start only after every
        in-place phase (cargo check, compile-fail, specguard) has ended.
        Does NOT prove anything about phases whose stubs do not log."""
        h = self.mut_harness(CHECK_SLEEP="2", MUT_SLEEP="2")
        proc = self.push(h)
        self.assertAllowed(proc, _out(proc))
        t = self.intervals(h)
        self.assertIn("MUT", t, _out(proc))
        self.assertIn("CHECK", t, _out(proc))
        for name, iv in t.items():
            if name == "MUT":
                continue
            self.assertGreaterEqual(t["MUT"]["start"], iv["end"],
                                    "mutation overlapped %s: %r" % (name, t))

    def test_m2_mutation_exit_1_blocks_and_is_not_recorded(self):
        """m2. In-place phases pass, mutation exits 1 => non-zero, no record.
        Does NOT prove the in-place phases' records exist."""
        h = self.mut_harness(MUT_EXIT="1")
        proc = self.push(h)
        self.assertBlocked(proc, _out(proc))
        self.assertTrue(h.pidfile.is_file(), "mutation stub never ran\n" + _out(proc))
        self.assertFalse(h.record_path(h.tree, MUT_ID).exists(), _out(proc))

    def test_m3_mutation_sigkill_blocks_and_is_not_recorded(self):
        """m3. Mutation process dies from SIGKILL => non-zero, no record."""
        h = self.mut_harness(MUT_KILL="1")
        proc = self.push(h)
        self.assertBlocked(proc, _out(proc))
        self.assertTrue(h.pidfile.is_file(), "mutation stub never ran\n" + _out(proc))
        self.assertFalse(h.record_path(h.tree, MUT_ID).exists(), _out(proc))

    def test_m4_fast_typecheck_failure_blocks_and_leaves_no_mutation_process(self):
        """m4. cargo check fails fast while the mutation stub would sleep 3s then
        write a marker. Hook exits non-zero and, once it returned, the mutation
        stub's pid (if it started) is gone. Does NOT assert whether the stub
        started or whether its result was recorded."""
        h = self.mut_harness(CHECK_EXIT="1", MUT_SLEEP="3")
        proc = self.push(h)
        self.assertBlocked(proc, _out(proc))
        if h.pidfile.is_file():
            pid = int(h.pidfile.read_text().strip())
            with self.assertRaises(ProcessLookupError, msg="mutation stub pid %d still alive after hook returned" % pid):
                os.kill(pid, 0)

    def test_m5_control_all_pass_allows_and_records(self):
        """m5. Anti-vacuity control: everything passes => exit 0 and the
        fail-open-mutation record exists."""
        h = self.mut_harness()
        proc = self.push(h)
        self.assertAllowed(proc, _out(proc))
        self.assertTrue(h.pidfile.is_file(), "mutation stub never ran\n" + _out(proc))
        self.assertTrue(h.record_path(h.tree, MUT_ID).is_file(), _out(proc))


if __name__ == "__main__":
    unittest.main()
