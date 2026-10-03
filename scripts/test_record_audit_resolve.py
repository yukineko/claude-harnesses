#!/usr/bin/env python3
"""Backlog 89544915 (T4, T5, T7) — scripts/record-audit.py closes its own
`record-audit:<dim>` review findings by OBSERVATION: a dimension that this very
run measured, successfully, as `ok`. Never by commit message, never because a
probe went quiet. Independent tests (author != implementer).

CONTRACT ASSUMED — the implementer MUST match these exactly:

* `main(...)` (the normal daily run, not --dry-run / --no-escalate) gains a
  resolve step next to `escalate()` (a function `resolve(dims, dry_run)` is the
  suggested shape, but only the CLI-level behaviour below is tested):
    - for each OPEN `record-audit:` finding in `overwatch review-queue --json`
      (rows keyed by `identifier`) whose dimension `d` has Dimension.state ==
      "ok" in this run, it shells out EXACTLY:
        overwatch record-disposition --finding-id <id> --verdict resolved
            --reviewer <non-empty> --evidence <non-empty, names the dim key>
            --observed-source <non-empty>
    - a breached dimension, or an undetermined (unmeasurable) dimension, closes
      nothing;
    - dry-run / --no-escalate record no disposition.
* Finding ids carry an EPISODE: `record-audit:<dim>:<first-breach-epoch>` where
  the epoch is the `--now` of the run that FIRST saw this breach after the last
  clean/closed state. A still-breached dimension on later runs keeps the SAME
  id (no new record-finding). After the finding is closed, a later breach gets a
  NEW id (new epoch) and is recorded again, so it is visible on the queue.
  (The episode must be persisted by the script, e.g. under
  RECORD_AUDIT_STATE_DIR; the test does not care where.)
  Legacy ids with NO episode suffix (`record-audit:<dim>`, already on live
  ledgers) are still closed when their dimension is ok.
* `--json` record gains `"resolved": [<finding ids closed this run>]` (always
  present, `[]` when none).
* Undetermined resolution (queue unreadable, record-disposition failing, or the
  dimension unmeasurable while a finding for it is open) => nothing closed for
  that finding, a line containing `NOT closed` appears on stdout or stderr, and
  the exit code is 2 (same as an unmeasurable dimension).
* overwatch is a stub on PATH (see _STUB). Unit under test is main().
"""

import importlib.util
import io
import json
import os
import re
import stat
import sys
import tempfile
import unittest
from contextlib import redirect_stderr, redirect_stdout
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_spec = importlib.util.spec_from_file_location("record_audit", _HERE / "record-audit.py")
ra = importlib.util.module_from_spec(_spec)
# Compiled from the SOURCE TEXT, deliberately not via the spec loader's
# exec_module: SourceFileLoader reuses a __pycache__ .pyc validated only by
# (source mtime at 1 s granularity, size), so a same-second size-preserving
# edit (e.g. a reordering mutant) would run stale bytecode -- a false GREEN /
# false mutation SURVIVOR, never a false red. Backlog 05726f9f; do not
# "simplify" this back. get_source() reads the .py, never the cache;
# dont_inherit keeps this file's __future__ flags off the subject.
exec(  # noqa: S102
    compile(_spec.loader.get_source(_spec.name), _spec.origin, "exec", dont_inherit=True),
    ra.__dict__,
)

DAY = 86400
NOW = 1785000000
DIM = "audit-convergence"  # M(1) = ok, M(0) = breach
EPISODE_ID = re.compile(r"^record-audit:audit-convergence:(\d+)$")


def M(value, **detail):
    return ra.Measurement.known(value, **detail)


def U(why):
    return ra.Measurement.undetermined(why)


# A stateful fake `overwatch`. Findings / dispositions live in $STUB_DIR; every
# call is appended to calls.jsonl. A second disposition for the same id is an
# idempotent no-op (first writer wins), like the real CLI.
_STUB = r'''#!{python}
import json, os, sys
d = os.environ["STUB_DIR"]
args = sys.argv[1:]
with open(os.path.join(d, "calls.jsonl"), "a") as fh:
    fh.write(json.dumps(args) + "\n")

def load(name):
    p = os.path.join(d, name)
    if not os.path.exists(p):
        return []
    return [json.loads(l) for l in open(p) if l.strip()]

def append(name, row):
    with open(os.path.join(d, name), "a") as fh:
        fh.write(json.dumps(row) + "\n")

def opt(flag):
    return args[args.index(flag) + 1] if flag in args else None

if os.path.exists(os.path.join(d, "FAIL_QUEUE")) and args[:1] == ["review-queue"]:
    sys.stderr.write("stub: review-queue unreadable\n")
    sys.exit(1)
if os.path.exists(os.path.join(d, "FAIL_DISPOSITION")) and args[:1] == ["record-disposition"]:
    sys.stderr.write("stub: store write failed\n")
    sys.exit(1)

cmd = args[0] if args else ""
if cmd == "review-queue":
    closed = {{r["finding_id"] for r in load("dispositions.jsonl")}}
    seen, rows = set(), []
    for f in load("findings.jsonl"):
        if f["finding_id"] in closed or f["finding_id"] in seen:
            continue
        seen.add(f["finding_id"])
        rows.append({{"kind": "ai-finding", "identifier": f["finding_id"]}})
    print(json.dumps(rows))
elif cmd == "record-finding":
    append("findings.jsonl", {{"finding_id": opt("--finding-id"), "source": opt("--source")}})
elif cmd == "record-disposition":
    fid = opt("--finding-id")
    if fid not in {{r["finding_id"] for r in load("dispositions.jsonl")}}:
        append("dispositions.jsonl", {{
            "finding_id": fid, "verdict": opt("--verdict"), "reviewer": opt("--reviewer"),
            "evidence": opt("--evidence"), "observed_source": opt("--observed-source"),
        }})
else:
    sys.stderr.write("stub: unsupported " + " ".join(args) + "\n")
    sys.exit(1)
'''


class _Base(unittest.TestCase):
    PROBES = (
        "measure_doc_drift",
        "measure_audit_convergence",
        "measure_open_review_queue",
        "measure_stale_undisposed",
        "measure_backlog_rot",
    )

    def setUp(self):
        self._saved = {n: getattr(ra, n) for n in self.PROBES}
        self._saved["_head_rev"] = ra._head_rev
        ra.measure_doc_drift = lambda: M(0)
        ra.measure_audit_convergence = lambda: M(1)
        ra.measure_open_review_queue = lambda: M(0)
        ra.measure_stale_undisposed = lambda: M(0)
        ra.measure_backlog_rot = lambda now, stale_days: M(0)
        ra._head_rev = lambda: "testrev"

        self._tmp = tempfile.TemporaryDirectory()
        root = Path(self._tmp.name)
        self.state = root / "state"
        self.stub = root / "stub"
        self.bin = root / "bin"
        for p in (self.state, self.stub, self.bin):
            p.mkdir()
        exe = self.bin / "overwatch"
        exe.write_text(_STUB.format(python=sys.executable))
        exe.chmod(exe.stat().st_mode | stat.S_IEXEC)

        self._env = {k: os.environ.get(k) for k in ("PATH", "STUB_DIR", "RECORD_AUDIT_STATE_DIR")}
        os.environ["PATH"] = f"{self.bin}{os.pathsep}{os.environ['PATH']}"
        os.environ["STUB_DIR"] = str(self.stub)
        os.environ["RECORD_AUDIT_STATE_DIR"] = str(self.state)

    def tearDown(self):
        for n, fn in self._saved.items():
            setattr(ra, n, fn)
        for k, v in self._env.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        self._tmp.cleanup()

    # -- helpers -----------------------------------------------------------
    def run_main(self, now, *argv):
        out, err = io.StringIO(), io.StringIO()
        with redirect_stdout(out), redirect_stderr(err):
            rc = ra.main(["--now", str(now), *argv])
        return rc, out.getvalue(), err.getvalue()

    def run_json(self, now, *argv):
        rc, out, err = self.run_main(now, "--json", *argv)
        return rc, json.loads(out), out + err

    def rows(self, name):
        p = self.stub / name
        if not p.exists():
            return []
        return [json.loads(l) for l in p.read_text().splitlines() if l.strip()]

    def findings(self):
        return [r["finding_id"] for r in self.rows("findings.jsonl")]

    def disps(self):
        return self.rows("dispositions.jsonl")

    def calls(self, cmd):
        return [c for c in self.rows("calls.jsonl") if c and c[0] == cmd]

    def breach(self):
        ra.measure_audit_convergence = lambda: M(0)

    def clean(self):
        ra.measure_audit_convergence = lambda: M(1)

    def open_breach(self, now=NOW):
        """Breach run at `now`; returns the recorded finding id (asserting it
        is an episode id)."""
        self.breach()
        rc, rec, txt = self.run_json(now)
        self.assertEqual(rc, 1, txt)
        ids = [i for i in self.findings() if i.startswith(f"record-audit:{DIM}")]
        self.assertEqual(len(ids), 1, f"exactly one finding recorded: {self.findings()}")
        return ids[0]


class T4_ClosesOnMeasuredOk(_Base):
    def test_ok_dimension_closes_its_open_finding_with_evidence(self):
        fid = self.open_breach(NOW)
        m = EPISODE_ID.match(fid)
        self.assertIsNotNone(m, f"id must carry an episode: {fid}")
        self.assertEqual(int(m.group(1)), NOW, "episode = first-breach --now")

        self.clean()
        rc, rec, txt = self.run_json(NOW + DAY)
        self.assertEqual(rc, 0, txt)
        self.assertEqual(rec["resolved"], [fid], rec)
        ds = self.disps()
        self.assertEqual(len(ds), 1, ds)
        d = ds[0]
        self.assertEqual(d["finding_id"], fid)
        self.assertEqual(d["verdict"], "resolved")
        self.assertTrue(d["reviewer"], d)
        self.assertTrue(d["observed_source"], d)
        self.assertIn(DIM, d["evidence"] or "", d)

    def test_still_breached_keeps_same_episode_id_and_does_not_rerecord(self):
        fid = self.open_breach(NOW)
        rc, rec, txt = self.run_json(NOW + DAY)  # still breach
        self.assertEqual(rc, 1, txt)
        self.assertEqual(self.findings(), [fid], "same episode, no duplicate record")
        self.assertEqual(self.disps(), [])

    def test_legacy_episodeless_open_finding_is_closed_when_dim_ok(self):
        """The live ledger already holds `record-audit:<dim>` ids with no
        episode; they must not be stranded forever."""
        (self.stub / "findings.jsonl").write_text(
            json.dumps({"finding_id": f"record-audit:{DIM}", "source": "record-audit"}) + "\n"
        )
        self.clean()
        rc, rec, txt = self.run_json(NOW)
        self.assertEqual(rc, 0, txt)
        self.assertEqual(rec["resolved"], [f"record-audit:{DIM}"], rec)
        self.assertEqual([d["verdict"] for d in self.disps()], ["resolved"])

    def test_only_the_ok_dimension_closes(self):
        """Control: a breached sibling dimension's finding stays open."""
        ra.measure_audit_convergence = lambda: M(0)
        ra.measure_doc_drift = lambda: M(10_000)  # breach too
        self.run_main(NOW)
        both = sorted(self.findings())
        self.assertEqual(len(both), 2, both)
        self.clean()  # convergence ok, doc-drift still breached
        rc, rec, txt = self.run_json(NOW + DAY)
        closed = [d["finding_id"] for d in self.disps()]
        self.assertEqual(len(closed), 1, closed)
        self.assertTrue(closed[0].startswith(f"record-audit:{DIM}"), closed)

    def test_dry_run_and_no_escalate_close_nothing(self):
        fid = self.open_breach(NOW)
        self.clean()
        for flag in ("--dry-run", "--no-escalate"):
            rc, rec, txt = self.run_json(NOW + DAY, flag)
            self.assertEqual(self.disps(), [], f"{flag} wrote a disposition")
        self.assertEqual(self.calls("record-disposition"), [])


class T5_NoCloseWhenBreachedOrUnmeasurable(_Base):
    def test_breach_closes_nothing(self):
        self.open_breach(NOW)
        rc, rec, txt = self.run_json(NOW + DAY)
        self.assertEqual(rc, 1, txt)
        self.assertEqual(rec.get("resolved", []), [], rec)
        self.assertEqual(self.disps(), [])
        self.assertEqual(self.calls("record-disposition"), [])

    def test_unmeasurable_dimension_closes_nothing_and_exits_two(self):
        self.open_breach(NOW)
        ra.measure_audit_convergence = lambda: U("overwatch audit-metrics down")
        rc, rec, txt = self.run_json(NOW + DAY)
        self.assertEqual(rc, 2, txt)
        self.assertEqual(rec.get("resolved", []), [], rec)
        self.assertEqual(self.disps(), [])
        self.assertEqual(self.calls("record-disposition"), [])
        self.assertIn("NOT closed", txt, "reason must be visible")

    def test_unreadable_queue_cannot_close_and_exits_two(self):
        fid = self.open_breach(NOW)
        self.clean()
        (self.stub / "FAIL_QUEUE").write_text("1")
        rc, rec, txt = self.run_json(NOW + DAY)
        self.assertEqual(rc, 2, txt)
        self.assertEqual(rec.get("resolved", []), [], rec)
        self.assertEqual(self.calls("record-disposition"), [])
        self.assertIn("NOT closed", txt)

    def test_failed_disposition_write_is_not_reported_resolved(self):
        fid = self.open_breach(NOW)
        self.clean()
        (self.stub / "FAIL_DISPOSITION").write_text("1")
        rc, rec, txt = self.run_json(NOW + DAY)
        self.assertEqual(rc, 2, txt)
        self.assertEqual(rec.get("resolved", []), [], rec)
        self.assertEqual(self.disps(), [])
        self.assertIn("NOT closed", txt)
        self.assertIn(fid, txt)


class T7_RecurrenceIsVisible(_Base):
    def test_rebreach_after_auto_close_is_a_new_open_finding(self):
        id1 = self.open_breach(NOW)
        self.clean()
        rc, rec, txt = self.run_json(NOW + DAY)
        self.assertEqual(rec["resolved"], [id1], txt)

        # Re-breach two days after the first breach.
        self.breach()
        rc, rec, txt = self.run_json(NOW + 2 * DAY)
        self.assertEqual(rc, 1, txt)
        ids = [i for i in self.findings() if i.startswith(f"record-audit:{DIM}")]
        self.assertEqual(len(ids), 2, f"a NEW finding must be recorded: {ids}")
        id2 = ids[1]
        self.assertNotEqual(id2, id1, "recurrence must not reuse the closed id")
        m = EPISODE_ID.match(id2)
        self.assertIsNotNone(m, id2)
        self.assertEqual(int(m.group(1)), NOW + 2 * DAY)

        # ...and it is OPEN on the queue (the disposition for id1 does not
        # hide it).
        import subprocess
        p = subprocess.run(
            ["overwatch", "review-queue", "--json"], capture_output=True, text=True
        )
        queue = [r["identifier"] for r in json.loads(p.stdout)]
        self.assertIn(id2, queue)
        self.assertNotIn(id1, queue)


if __name__ == "__main__":
    unittest.main(verbosity=2)
