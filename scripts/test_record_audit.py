#!/usr/bin/env python3
"""Tests for scripts/record-audit.py (backlog 0f55003a).

The thing under test is a job that exists to catch OTHER records reading as
clean when they are not. So the bar here is specifically: can a broken probe,
an empty result, or a failed write reach a passing exit code? Every probe gets
a fault injected, and every "this fails" assertion is paired with a control
proving the assertion is about the fault and not about the fixture.
"""

import importlib.util
import io
import json
import os
import subprocess
import sys
import tempfile
import unittest
from contextlib import redirect_stdout
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("record_audit", _HERE / "record-audit.py")
ra = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(ra)

DAY = 86400
NOW = 1785000000


def M(value, **detail):
    return ra.Measurement.known(value, **detail)


def U(why):
    return ra.Measurement.undetermined(why)


class _Patched(unittest.TestCase):
    """Swaps the probe functions for stubs, restores them after."""

    PROBES = (
        "measure_doc_drift",
        "measure_audit_convergence",
        "measure_open_review_queue",
        "measure_stale_undisposed",
        "measure_backlog_rot",
    )

    def setUp(self):
        self._saved = {name: getattr(ra, name) for name in self.PROBES}
        self._saved["escalate"] = ra.escalate
        self._saved["_head_rev"] = ra._head_rev
        # Clean by default, so any failure in a case is caused by what that case
        # changed rather than inherited from the real repo.
        ra.measure_doc_drift = lambda: M(0)
        ra.measure_audit_convergence = lambda: M(1)
        ra.measure_open_review_queue = lambda: M(0)
        ra.measure_stale_undisposed = lambda: M(0)
        ra.measure_backlog_rot = lambda now, stale_days: M(0)
        ra.escalate = lambda dims, dry_run: ([], "")
        ra._head_rev = lambda: "testrev"
        self._tmp = tempfile.TemporaryDirectory()
        self._saved_env = os.environ.get("RECORD_AUDIT_STATE_DIR")
        os.environ["RECORD_AUDIT_STATE_DIR"] = self._tmp.name

    def tearDown(self):
        for name, fn in self._saved.items():
            setattr(ra, name, fn)
        if self._saved_env is None:
            os.environ.pop("RECORD_AUDIT_STATE_DIR", None)
        else:
            os.environ["RECORD_AUDIT_STATE_DIR"] = self._saved_env
        self._tmp.cleanup()

    def run_main(self, *argv):
        out = io.StringIO()
        with redirect_stdout(out):
            rc = ra.main(["--now", str(NOW), *argv])
        return rc, out.getvalue()


class ExitCodes(_Patched):
    def test_all_clean_exits_zero(self):
        """Anti-vacuity control for every case below: the clean fixture passes,
        so a non-zero elsewhere is caused by the injected fault."""
        rc, out = self.run_main()
        self.assertEqual(rc, 0, out)

    def test_a_breach_exits_one(self):
        ra.measure_audit_convergence = lambda: M(0)
        rc, out = self.run_main()
        self.assertEqual(rc, 1, out)
        self.assertIn("audit-convergence", out)

    def test_an_unmeasurable_dimension_exits_two_not_zero(self):
        """The core property. A probe that could not answer must not be spent as
        a passing dimension — otherwise a broken overwatch reads exactly like a
        healthy ledger, which is the fault this whole job exists to detect."""
        ra.measure_audit_convergence = lambda: U("overwatch not on PATH")
        rc, out = self.run_main()
        self.assertEqual(rc, 2, out)
        self.assertIn("COULD NOT MEASURE", out)

    def test_undetermined_outranks_breach(self):
        """With both present the exit must be 2. If breach won, a run that could
        not measure anything at all but happened to trip one threshold would
        report the narrower problem and hide the wider one."""
        ra.measure_audit_convergence = lambda: M(0)
        ra.measure_doc_drift = lambda: U("check-doc-claims crashed")
        rc, out = self.run_main()
        self.assertEqual(rc, 2, out)

    def test_every_probe_can_independently_force_exit_two(self):
        """Each dimension is wired to the exit code, not just the first one.
        Without this, a single correctly-wired probe would carry the suite.

        NB: restores only the ONE probe it swapped. Re-entering setUp/tearDown
        here would make unittest's own tearDown restore the stubs as if they
        were the originals, leaking them into every later class — which it did,
        and which showed up as eight unrelated failures in Probes."""
        for name in self.PROBES:
            with self.subTest(probe=name):
                clean = getattr(ra, name)
                try:
                    if name == "measure_backlog_rot":
                        setattr(ra, name, lambda now, stale_days: U("probe down"))
                    else:
                        setattr(ra, name, lambda: U("probe down"))
                    rc, out = self.run_main()
                    self.assertEqual(rc, 2, f"{name} did not reach the exit code\n{out}")
                finally:
                    setattr(ra, name, clean)


class MeasurementType(unittest.TestCase):
    def test_a_measurement_cannot_be_both(self):
        with self.assertRaises(ValueError):
            ra.Measurement(value=1, why="broken")

    def test_a_measurement_cannot_be_neither(self):
        """The empty Measurement is the one that would default to a number."""
        with self.assertRaises(ValueError):
            ra.Measurement()

    def test_undetermined_carries_no_value(self):
        m = U("nope")
        self.assertFalse(m.is_known)
        self.assertIsNone(m.value)

    def test_zero_is_a_real_value_not_an_absence(self):
        m = M(0)
        self.assertTrue(m.is_known)
        self.assertEqual(m.value, 0)


class RecordRoot(unittest.TestCase):
    """Which project's records get read.

    Written after observing the failure, not before predicting it: run from a
    linked worktree, this job reported `pending_total: 0` and `review-queue
    depth: 0` against a live 87-item queue and a 42-row queue, because overwatch
    and backlog resolve their stores from the cwd. Every store-backed dimension
    read clean off a store that simply was not there.
    """

    def setUp(self):
        self._saved = ra._run

    def tearDown(self):
        ra._run = self._saved

    def test_a_linked_worktree_resolves_to_the_main_worktree(self):
        """git rev-parse --git-common-dir points at the MAIN worktree's .git
        from inside a linked one, which is where the records live."""
        with tempfile.TemporaryDirectory() as tmp:
            main = Path(tmp, "main")
            main.mkdir()
            subprocess.run(["git", "init", "-q"], cwd=main, check=True)
            subprocess.run(["git", "config", "user.email", "t@t.t"], cwd=main, check=True)
            subprocess.run(["git", "config", "user.name", "t"], cwd=main, check=True)
            Path(main, "f").write_text("x")
            subprocess.run(["git", "add", "-A"], cwd=main, check=True)
            subprocess.run(["git", "commit", "-qm", "base"], cwd=main, check=True)
            linked = Path(tmp, "linked")
            subprocess.run(
                ["git", "worktree", "add", "-q", str(linked), "-b", "wt"],
                cwd=main, check=True, capture_output=True,
            )
            saved_repo = ra.REPO
            ra.REPO = str(linked)
            try:
                got = ra.record_root()
            finally:
                ra.REPO = saved_repo
        self.assertEqual(
            os.path.realpath(got), os.path.realpath(str(main)),
            "a linked worktree must resolve to the record-owning main worktree",
        )

    def test_the_main_worktree_resolves_to_itself(self):
        """Control: the fix must not redirect a normal checkout somewhere else."""
        with tempfile.TemporaryDirectory() as tmp:
            main = Path(tmp, "solo")
            main.mkdir()
            subprocess.run(["git", "init", "-q"], cwd=main, check=True)
            saved_repo = ra.REPO
            ra.REPO = str(main)
            try:
                got = ra.record_root()
            finally:
                ra.REPO = saved_repo
        self.assertEqual(os.path.realpath(got), os.path.realpath(str(main)))

    def test_an_unresolvable_root_raises_rather_than_falling_back_to_repo(self):
        """Falling back to REPO is how the wrong store gets read confidently.
        There is no defensible guess here, so there is no guess."""
        ra._run = lambda cmd, cwd=None, timeout=180: (128, "", "not a git repository")
        self.assertIsNone(ra.record_root())
        with self.assertRaises(RuntimeError) as cm:
            ra._store_cwd()
        self.assertIn("refusing", str(cm.exception))

    def test_a_probe_reports_undetermined_when_the_root_is_unresolvable(self):
        """End of that chain: undetermined, never 0."""
        ra._run = lambda cmd, cwd=None, timeout=180: (128, "", "not a git repository")
        m = ra.measure_backlog_rot(NOW, 21)
        self.assertFalse(m.is_known, f"expected undetermined, got {m!r}")

    def test_escalation_refuses_to_record_when_the_root_is_unresolvable(self):
        """already_open() degrades to None, and None must not append."""
        ra._run = lambda cmd, cwd=None, timeout=180: (128, "", "not a git repository")
        self.assertIsNone(ra.already_open("record-audit:whatever"))


class RunJson(unittest.TestCase):
    """`_run_json`'s own contract: any failure RAISES, never returns {}.

    These exist because mutation testing found that returning {} on a parse
    failure left the whole suite green — every probe happens to re-catch the
    empty dict via its own shape check, so the assertion in
    `test_unparseable_output_is_undetermined` was riding on a downstream guard
    rather than on the behaviour its name describes. Defence in depth is good;
    a test whose name claims one layer and exercises another is not. So the
    contract is pinned where it lives, and the probe-level cases stay as the
    second layer.
    """

    def setUp(self):
        self._saved = ra._run

    def tearDown(self):
        ra._run = self._saved

    def test_unparseable_stdout_raises(self):
        ra._run = lambda cmd, cwd=None, timeout=180: (0, "<html>nope", "")
        with self.assertRaises(RuntimeError) as cm:
            ra._run_json(["fake"])
        self.assertIn("parseable", str(cm.exception))

    def test_empty_stdout_raises(self):
        """The specific shape a crashed tool produces."""
        ra._run = lambda cmd, cwd=None, timeout=180: (0, "", "")
        with self.assertRaises(RuntimeError):
            ra._run_json(["fake"])

    def test_nonzero_exit_raises_even_with_valid_json(self):
        """The exit status is part of the answer. A tool that printed a usable
        object and then failed has not necessarily finished its work."""
        ra._run = lambda cmd, cwd=None, timeout=180: (1, '{"ok": true}', "partial")
        with self.assertRaises(RuntimeError) as cm:
            ra._run_json(["fake"])
        self.assertIn("exited 1", str(cm.exception))

    def test_valid_json_and_zero_exit_returns_the_object(self):
        """Control: the three above must be about the FAILURE, not about
        _run_json raising unconditionally."""
        ra._run = lambda cmd, cwd=None, timeout=180: (0, '{"a": [1, 2]}', "")
        self.assertEqual(ra._run_json(["fake"]), {"a": [1, 2]})


class Probes(unittest.TestCase):
    """Fault injection at the subprocess boundary.

    `_store_cwd` is stubbed out here so the single `_run` stub does not also
    have to answer the `git rev-parse` that resolves the record root — which
    dimension reads which store is RecordRoot's subject, not this class's.
    """

    def setUp(self):
        self._saved = ra._run
        self._saved_json = ra._run_json
        self._saved_cwd = ra._store_cwd
        ra._store_cwd = lambda: "/tmp"

    def tearDown(self):
        ra._run = self._saved
        ra._run_json = self._saved_json
        ra._store_cwd = self._saved_cwd

    def test_nonzero_exit_is_undetermined_not_empty(self):
        """A probe that exits non-zero with empty stdout must not parse as
        'checked, found nothing'. This is the exit-code-ignored fail-open."""
        ra._run = lambda cmd, cwd=None, timeout=180: (3, "", "boom")
        m = ra.measure_audit_convergence()
        self.assertFalse(m.is_known, f"expected undetermined, got {m!r}")
        self.assertIn("exited 3", m.why)

    def test_unparseable_output_is_undetermined(self):
        """Second layer over RunJson: even if the parse guard were removed, the
        probe's own shape check must still refuse to call {} an answer."""
        ra._run = lambda cmd, cwd=None, timeout=180: (0, "not json at all", "")
        m = ra.measure_open_review_queue()
        self.assertFalse(m.is_known, f"expected undetermined, got {m!r}")

    def test_missing_binary_is_undetermined(self):
        """Injected at the subprocess boundary rather than at _run, on purpose:
        the translation from FileNotFoundError to a reported reason lives INSIDE
        _run, so stubbing _run would have skipped the code under test and let the
        raw exception escape the probe uncaught (it did — that is why this case
        is written this way)."""
        real = ra.subprocess

        class _Shim:
            TimeoutExpired = subprocess.TimeoutExpired

            @staticmethod
            def run(*a, **kw):
                raise FileNotFoundError(a[0][0])

        ra.subprocess = _Shim
        try:
            m = ra.measure_stale_undisposed()
        finally:
            ra.subprocess = real
        self.assertFalse(m.is_known, f"expected undetermined, got {m!r}")
        self.assertIn("PATH", m.why)

    def test_a_probe_timeout_is_undetermined(self):
        """A hung probe must not be reported as a healthy record either."""
        real = ra.subprocess

        class _Shim:
            TimeoutExpired = subprocess.TimeoutExpired

            @staticmethod
            def run(*a, **kw):
                raise subprocess.TimeoutExpired(a[0], 180)

        ra.subprocess = _Shim
        try:
            m = ra.measure_open_review_queue()
        finally:
            ra.subprocess = real
        self.assertFalse(m.is_known, f"expected undetermined, got {m!r}")
        self.assertIn("timed out", m.why)

    def test_valid_probe_output_is_known(self):
        """Control: the three cases above must be about the FAULT. Without this,
        a measure_* that returned Undetermined unconditionally would pass them
        all."""
        ra._run = lambda cmd, cwd=None, timeout=180: (
            0, json.dumps({"stale_undisposed_with_fix_commit": 4, "total": 9}), ""
        )
        m = ra.measure_stale_undisposed()
        self.assertTrue(m.is_known, f"expected known, got {m!r}")
        self.assertEqual(m.value, 4)

    def test_converging_null_is_undetermined_not_converging(self):
        """overwatch's own third state. Reading 'not enough rounds to say' as
        converging would make the flag reachable by having no history."""
        ra._run = lambda cmd, cwd=None, timeout=180: (
            0, json.dumps({"converging": None, "rounds": [{"new_findings": 1}]}), ""
        )
        m = ra.measure_audit_convergence()
        self.assertFalse(m.is_known, f"expected undetermined, got {m!r}")
        self.assertIn("unknown", m.why)

    def test_converging_false_is_a_measured_zero(self):
        """Control for the case above: false must NOT be undetermined. Conflating
        them would let a genuinely non-converging audit hide behind 'unknown'."""
        ra._run = lambda cmd, cwd=None, timeout=180: (
            0, json.dumps({"converging": False, "rounds": []}), ""
        )
        m = ra.measure_audit_convergence()
        self.assertTrue(m.is_known, f"expected known, got {m!r}")
        self.assertEqual(m.value, 0)

    def test_missing_findings_key_is_undetermined_not_zero_drift(self):
        """A schema change in check-doc-claims.py must not read as 'no drift'."""
        ra._run = lambda cmd, cwd=None, timeout=180: (
            0, json.dumps({"verdict": "clean"}), ""
        )
        m = ra.measure_doc_drift()
        self.assertFalse(m.is_known, f"expected undetermined, got {m!r}")
        self.assertIn("shape change", m.why)

    def test_doc_drift_counts_exempt_findings(self):
        """The exempt population IS the finding. Counting only non-exempt ones
        would report 0 against the 140 stale claims measured in this repo."""
        payload = json.dumps({
            "verdict": "clean",
            "findings": [
                {"kind": "line-drifted", "exempt": True},
                {"kind": "quote-not-found", "exempt": True},
            ],
        })
        ra._run = lambda cmd, cwd=None, timeout=180: (0, payload, "")
        m = ra.measure_doc_drift()
        self.assertTrue(m.is_known, f"expected known, got {m!r}")
        # Two probes (docs + CLAUDE.md) share the stub, hence 4.
        self.assertEqual(m.value, 4)


class BacklogRot(unittest.TestCase):
    def setUp(self):
        self._saved = ra._run
        self._saved_cwd = ra._store_cwd
        ra._store_cwd = lambda: "/tmp"

    def tearDown(self):
        ra._run = self._saved
        ra._store_cwd = self._saved_cwd

    def _stub(self, pending, failed):
        def _fn(cmd, cwd=None, timeout=180):
            which = pending if "pending" in cmd else failed
            return (0, json.dumps(which), "")
        ra._run = _fn

    def test_long_pending_item_is_counted(self):
        self._stub([{"id": "a", "created_at": NOW - 40 * DAY, "title": "old"}], [])
        m = ra.measure_backlog_rot(NOW, 21)
        self.assertEqual(m.value, 1, repr(m.detail))
        self.assertEqual(m.detail["stale_count"], 1)

    def test_fresh_item_is_not_counted(self):
        """Control: the case above must be about the AGE."""
        self._stub([{"id": "a", "created_at": NOW - 2 * DAY, "title": "new"}], [])
        m = ra.measure_backlog_rot(NOW, 21)
        self.assertEqual(m.value, 0, repr(m.detail))

    def test_refailed_resurfacer_is_counted(self):
        """`backlog fail` defers rather than retires, so an item can be failed,
        come back after two days, and be failed again. updated_at past the defer
        window while still `failed` is that trace."""
        self._stub([], [{"id": "b", "created_at": NOW - 10 * DAY,
                         "updated_at": NOW - 1 * DAY, "title": "keeps coming back"}])
        m = ra.measure_backlog_rot(NOW, 21)
        self.assertEqual(m.detail["refailed_count"], 1, repr(m.detail))

    def test_failed_once_within_the_defer_window_is_not_counted(self):
        """Control: a plain single failure is not a resurfacer."""
        self._stub([], [{"id": "b", "created_at": NOW - 10 * DAY,
                         "updated_at": NOW - 10 * DAY + 60, "title": "failed once"}])
        m = ra.measure_backlog_rot(NOW, 21)
        self.assertEqual(m.detail["refailed_count"], 0, repr(m.detail))

    def test_missing_created_at_is_undetermined_not_fresh(self):
        """An item with no timestamp cannot be judged. Treating it as age 0 would
        make a corrupt queue the freshest one."""
        self._stub([{"id": "a", "title": "no timestamp"}], [])
        m = ra.measure_backlog_rot(NOW, 21)
        self.assertFalse(m.is_known, f"expected undetermined, got {m!r}")


class Escalation(unittest.TestCase):
    def setUp(self):
        self._saved_run = ra._run
        self._saved_open = ra.already_open
        self.calls = []

    def tearDown(self):
        ra._run = self._saved_run
        ra.already_open = self._saved_open

    def _dims(self):
        return [ra.Dimension("audit-convergence", "t", M(0), 1, True, "high")]

    def _record_stub(self, rc=0):
        def _fn(cmd, cwd=None, timeout=180):
            self.calls.append(cmd)
            return (rc, "", "denied" if rc else "")
        ra._run = _fn

    def test_a_breach_is_recorded_to_the_review_queue(self):
        ra.already_open = lambda fid: False
        self._record_stub()
        ids, note = ra.escalate(self._dims(), dry_run=False)
        self.assertEqual(ids, ["record-audit:audit-convergence"], note)
        self.assertEqual(len(self.calls), 1)
        self.assertIn("record-finding", self.calls[0])
        self.assertIn("--verdict", self.calls[0])

    def test_an_already_open_finding_is_not_duplicated(self):
        """record-finding is a plain append (overwatch store.rs), so a daily
        re-record of an unchanged condition would stack a row a day and bury the
        surface this job exists to keep readable."""
        ra.already_open = lambda fid: True
        self._record_stub()
        ids, note = ra.escalate(self._dims(), dry_run=False)
        self.assertEqual(ids, [])
        self.assertEqual(self.calls, [], "must not shell out at all")
        self.assertIn("already open", note)

    def test_an_unreadable_queue_does_not_record_and_says_so(self):
        """Undetermined openness resolves away from appending: the duplicate is
        the harm, and there is no evidence the record was needed."""
        ra.already_open = lambda fid: None
        self._record_stub()
        ids, note = ra.escalate(self._dims(), dry_run=False)
        self.assertEqual(ids, [])
        self.assertEqual(self.calls, [])
        self.assertIn("COULD NOT RECORD", note)

    def test_a_failed_record_is_reported_not_swallowed(self):
        ra.already_open = lambda fid: False
        self._record_stub(rc=1)
        ids, note = ra.escalate(self._dims(), dry_run=False)
        self.assertEqual(ids, [])
        self.assertIn("COULD NOT RECORD", note)

    def test_a_clean_dimension_is_never_recorded(self):
        ra.already_open = lambda fid: False
        self._record_stub()
        clean = [ra.Dimension("audit-convergence", "t", M(1), 1, False, "high")]
        ids, note = ra.escalate(clean, dry_run=False)
        self.assertEqual(ids, [])
        self.assertEqual(self.calls, [])

    def test_an_undetermined_dimension_is_not_recorded_as_a_breach(self):
        """It is reported via the exit code and the report text instead —
        recording it as a confirmed finding would assert something unmeasured."""
        ra.already_open = lambda fid: False
        self._record_stub()
        undet = [ra.Dimension("audit-convergence", "t", U("down"), 1, False, "high")]
        ids, note = ra.escalate(undet, dry_run=False)
        self.assertEqual(ids, [])
        self.assertEqual(self.calls, [])


class ObservationLedger(_Patched):
    def test_a_run_appends_one_observation(self):
        rc, _ = self.run_main()
        self.assertEqual(rc, 0)
        rows = Path(self._tmp.name, "observations.jsonl").read_text().splitlines()
        self.assertEqual(len(rows), 1)
        rec = json.loads(rows[0])
        self.assertEqual(rec["ts"], NOW)
        self.assertEqual(len(rec["dimensions"]), 5)

    def test_a_second_run_appends_rather_than_replaces(self):
        """The ledger is the trend. A truncating write would leave exactly one
        observation forever and the convergence question unanswerable."""
        self.run_main()
        self.run_main()
        rows = Path(self._tmp.name, "observations.jsonl").read_text().splitlines()
        self.assertEqual(len(rows), 2)

    def test_an_unwritable_ledger_exits_two(self):
        """A run that left no record did not produce the trend it exists for."""
        os.environ["RECORD_AUDIT_STATE_DIR"] = "/dev/null/not-a-dir"
        rc, out = self.run_main()
        self.assertEqual(rc, 2, out)
        self.assertIn("could not append", out)

    def test_dry_run_writes_nothing(self):
        rc, _ = self.run_main("--dry-run")
        self.assertFalse(Path(self._tmp.name, "observations.jsonl").exists())

    def test_json_mode_reports_the_same_verdict(self):
        ra.measure_audit_convergence = lambda: M(0)
        rc, out = self.run_main("--json")
        self.assertEqual(rc, 1)
        rec = json.loads(out)
        self.assertEqual(rec["breached"], ["audit-convergence"])


class EndToEnd(unittest.TestCase):
    """Runs the real script as a subprocess against the real repo."""

    def test_it_runs_and_reports_without_touching_the_queue(self):
        with tempfile.TemporaryDirectory() as tmp:
            env = dict(os.environ, RECORD_AUDIT_STATE_DIR=tmp)
            p = subprocess.run(
                [sys.executable, str(_HERE / "record-audit.py"), "--json", "--no-escalate"],
                capture_output=True, text=True, env=env, timeout=300,
            )
        self.assertIn(p.returncode, (0, 1, 2), p.stderr)
        rec = json.loads(p.stdout)
        self.assertEqual(len(rec["dimensions"]), 5)
        for d in rec["dimensions"]:
            self.assertIn(d["state"], ("ok", "breach", "undetermined"))

    def test_the_daily_stanza_names_this_repo(self):
        p = subprocess.run(
            [sys.executable, str(_HERE / "record-audit.py"), "--print-daily-task"],
            capture_output=True, text=True, timeout=60,
        )
        self.assertEqual(p.returncode, 0, p.stderr)
        self.assertIn("[[task]]", p.stdout)
        self.assertIn('name = "record-audit"', p.stdout)
        self.assertIn(str(_HERE.parent), p.stdout)


if __name__ == "__main__":
    unittest.main(verbosity=2)
