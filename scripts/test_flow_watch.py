#!/usr/bin/env python3
"""Tests for `crates/flow/bin/flow-watch` (spec: crates/flow/WATCH-SPEC.md).

Written by an independent test author (not the implementer), RED first.

Each test runs the real launcher as a subprocess in a temp dir with fake
`condukt` / `backlog` executables (FLOW_WATCH_CONDUKT / FLOW_WATCH_BACKLOG),
small --poll-secs/--max-secs values, and reads the stdout event lines.

What these tests do NOT prove:
  * Timing under load: ages/idle are driven by fabricated `ts` / os.utime, and
    waits are a few seconds; a loaded machine can only delay, not reorder, but
    the tests do not measure the real poll cadence or the 600s/1700s defaults.
  * Default-path behaviour (~/.blastguard/relay/<SESSION>.jsonl and
    ~/.condukt/state/*/) -- every test passes explicit paths.
  * Which run-file statuses are "terminal" beyond `done`; only `running` and
    `pending`(non-terminal) vs `done`(terminal) are exercised.
  * Real `condukt` / `backlog` argument validity: the fake logs argv, and the
    tests only check the documented subcommand words appear.
  * Exact wording of WATCH-ERROR reasons (only the `WATCH-ERROR ` prefix and a
    mention of the offending file/line).
  * Monitor-tool integration, SKILL.md wording, the version bump, or the
    git-index exec bit of the launcher.
  * Multi-session isolation, very large queues, or concurrent writers.
"""
import json
import os
import shutil
import stat
import subprocess
import tempfile
import time
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FLOW_WATCH = os.path.join(ROOT, "crates", "flow", "bin", "flow-watch")
SESSION = "sess-test"


def _fake(path, rc, log):
    with open(path, "w") as f:
        f.write('#!/bin/sh\necho "$@" >> "%s"\nexit %d\n' % (log, rc))
    os.chmod(path, 0o755)


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="flowwatch-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.queue = os.path.join(self.tmp, "relay.jsonl")
        self.state = os.path.join(self.tmp, "state")
        os.makedirs(self.state)
        self.project = os.path.join(self.tmp, "proj")
        os.makedirs(self.project)
        self.clog = os.path.join(self.tmp, "condukt.log")
        self.blog = os.path.join(self.tmp, "backlog.log")
        self.condukt = os.path.join(self.tmp, "condukt")
        self.backlog = os.path.join(self.tmp, "backlog")
        _fake(self.condukt, 0, self.clog)
        _fake(self.backlog, 0, self.blog)

    # -- helpers
    def write_queue(self, rows, raw_lines=None):
        with open(self.queue, "w") as f:
            for r in rows:
                f.write(json.dumps(r) + "\n")
            for l in raw_lines or []:
                f.write(l + "\n")

    def append_queue(self, row):
        with open(self.queue, "a") as f:
            f.write(json.dumps(row) + "\n")

    @staticmethod
    def queued(id_, ts=None, agent="agent-1", cwd="/work/x"):
        return {"event": "queued", "id": id_, "ts": int(ts if ts is not None else time.time()),
                "session_id": SESSION, "agent_id": agent, "cwd": cwd,
                "command": "ls", "reason": "r"}

    def args(self, **kw):
        a = [FLOW_WATCH, "--session", SESSION, "--project", self.project,
             "--relay-queue", self.queue, "--condukt-state", self.state,
             "--poll-secs", "0.2", "--state-secs", "0.3", "--heartbeat-secs", "600",
             "--relay-stall-secs", "120", "--task-stall-secs", "1200",
             "--max-secs", "3"]
        for k, v in kw.items():
            a[a.index("--" + k.replace("_", "-")) + 1] = str(v)
        return a

    def env(self, **extra):
        e = dict(os.environ)
        e["FLOW_WATCH_CONDUKT"] = self.condukt
        e["FLOW_WATCH_BACKLOG"] = self.backlog
        e.update(extra)
        return e

    def run_watch(self, args=None, env=None, timeout=30):
        p = subprocess.run(args or self.args(), env=env or self.env(),
                           capture_output=True, text=True, timeout=timeout)
        return p, [l for l in p.stdout.splitlines() if l.strip()]

    def write_run(self, run_id, tasks, age=0):
        path = os.path.join(self.state, "%s.json" % run_id)
        with open(path, "w") as f:
            json.dump({"run_id": run_id, "tasks": [{"id": i, "status": s} for i, s in tasks]}, f)
        if age:
            t = time.time() - age
            os.utime(path, (t, t))
        return path


class TestRelay(Base):
    def test_pending_at_startup_emits_relay(self):
        self.write_queue([self.queued("bgr-1", agent="ag9", cwd="/w/q")])
        p, lines = self.run_watch()
        self.assertIn("RELAY bgr-1 agent=ag9 cwd=/w/q", lines,
                      "a queued row with no adjudication, already present at startup, must be reported exactly once in the grammar")
        self.assertEqual(sum(1 for l in lines if l.startswith("RELAY bgr-1 ")), 1,
                         "RELAY is emitted once per id, not once per poll")

    def test_new_row_appended_while_running_emits_relay(self):
        self.write_queue([])
        proc = subprocess.Popen(self.args(max_secs=4), env=self.env(),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            time.sleep(1.0)
            self.append_queue(self.queued("bgr-late", agent="ag2", cwd="/w/z"))
            out, _ = proc.communicate(timeout=30)
        finally:
            if proc.poll() is None:
                proc.kill()
        self.assertIn("RELAY bgr-late agent=ag2 cwd=/w/z", out.splitlines(),
                      "a queued row appended after startup must be picked up by polling")

    def test_executed_and_dismissed_ids_are_not_relayed(self):
        self.write_queue([
            self.queued("bgr-ex"), {"event": "executed", "id": "bgr-ex", "ts": int(time.time())},
            self.queued("bgr-di"), {"event": "dismissed", "id": "bgr-di", "ts": int(time.time())},
            self.queued("bgr-open"),
        ])
        _, lines = self.run_watch()
        self.assertFalse(any("bgr-ex" in l or "bgr-di" in l for l in lines),
                         "ids with a later executed/dismissed row are adjudicated and must produce no event")
        self.assertTrue(any(l.startswith("RELAY bgr-open ") for l in lines),
                        "control: the still-pending id in the same file is reported")

    def test_relay_stall_by_age(self):
        old = time.time() - 500
        self.write_queue([self.queued("bgr-old", ts=old), self.queued("bgr-new")])
        _, lines = self.run_watch()
        stalls = [l for l in lines if l.startswith("RELAY-STALL ")]
        self.assertEqual([l.split()[1] for l in stalls], ["bgr-old"],
                         "only the unadjudicated relay older than --relay-stall-secs stalls; the fresh one does not")
        self.assertRegex(stalls[0], r"^RELAY-STALL bgr-old age=\d+s$",
                         "RELAY-STALL line matches the documented grammar")
        self.assertGreaterEqual(int(stalls[0].split("age=")[1].rstrip("s")), 500,
                                "age is computed from the row ts")

    def test_relay_stall_not_repeated_within_window(self):
        self.write_queue([self.queued("bgr-old", ts=time.time() - 500)])
        _, lines = self.run_watch()
        self.assertEqual(sum(1 for l in lines if l.startswith("RELAY-STALL bgr-old")), 1,
                         "RELAY-STALL is re-emitted at most once per --relay-stall-secs")

    def test_missing_queue_file_is_not_an_error(self):
        self.assertFalse(os.path.exists(self.queue))
        p, lines = self.run_watch()
        self.assertFalse(any(l.startswith("WATCH-ERROR") for l in lines),
                         "a queue file that does not exist yet means no relays, not an error")
        self.assertEqual(lines[-1], "EXPIRED re-arm", "run completes normally")

    def test_unparsable_queue_line_is_watch_error(self):
        self.write_queue([self.queued("bgr-1")], raw_lines=["{not json at all"])
        _, lines = self.run_watch()
        self.assertTrue(any(l.startswith("WATCH-ERROR ") for l in lines),
                        "an unparsable queue line must be surfaced, never silently skipped (fail-closed)")
        self.assertTrue(any(l.startswith("RELAY bgr-1 ") for l in lines),
                        "a bad line must not hide the valid pending relay next to it")

    @unittest.skipIf(os.geteuid() == 0, "root can read mode-000 files")
    def test_unreadable_existing_queue_is_watch_error(self):
        self.write_queue([self.queued("bgr-1")])
        os.chmod(self.queue, 0)
        self.addCleanup(os.chmod, self.queue, 0o644)
        _, lines = self.run_watch()
        self.assertTrue(any(l.startswith("WATCH-ERROR ") for l in lines),
                        "an existing but unreadable queue file is an error, not 'no relays'")


class TestState(Base):
    def test_state_at_startup_for_nonterminal_run(self):
        self.write_run("run-a", [("t1", "running"), ("t2", "pending")])
        _, lines = self.run_watch()
        st = [l for l in lines if l.startswith("STATE run-a ")]
        self.assertTrue(st, "a run with a non-terminal task is reported once at startup")
        self.assertIn("t1=running", st[0], "STATE lists task=status pairs")
        self.assertIn("t2=pending", st[0], "STATE lists every task")

    def test_no_startup_state_for_all_terminal_run(self):
        self.write_run("run-done", [("t1", "done")])
        _, lines = self.run_watch()
        self.assertFalse(any(l.startswith("STATE run-done") for l in lines),
                         "runs whose tasks are all terminal are not reported at startup")

    def test_state_emitted_on_change(self):
        self.write_run("run-b", [("t1", "running")])
        proc = subprocess.Popen(self.args(max_secs=4), env=self.env(),
                                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
        try:
            time.sleep(1.2)
            self.write_run("run-b", [("t1", "done")])
            out, _ = proc.communicate(timeout=30)
        finally:
            if proc.poll() is None:
                proc.kill()
        sts = [l for l in out.splitlines() if l.startswith("STATE run-b ")]
        self.assertGreaterEqual(len(sts), 2, "startup STATE plus a STATE when the status vector changes")
        self.assertIn("t1=done", sts[-1], "the change line carries the new status")

    def test_unchanged_state_not_repeated(self):
        self.write_run("run-c", [("t1", "running")])
        _, lines = self.run_watch()
        self.assertEqual(sum(1 for l in lines if l.startswith("STATE run-c ")), 1,
                         "STATE is emitted only on change, not every --state-secs")

    def test_unreadable_run_file_is_watch_error(self):
        with open(os.path.join(self.state, "run-bad.json"), "w") as f:
            f.write("{garbage")
        _, lines = self.run_watch()
        self.assertTrue(any(l.startswith("WATCH-ERROR ") for l in lines),
                        "an unparsable run file must be surfaced, not skipped silently")


class TestTaskStall(Base):
    def test_stall_when_run_file_and_journal_old(self):
        p = self.write_run("run-s", [("t1", "running")], age=3000)
        j = os.path.join(self.state, "run-s.journal.jsonl")
        open(j, "w").write("{}\n")
        t = time.time() - 3000
        os.utime(j, (t, t))
        _, lines = self.run_watch()
        st = [l for l in lines if l.startswith("TASK-STALL ")]
        self.assertEqual(len(st), 1, "exactly one TASK-STALL for the idle running task within the window")
        self.assertRegex(st[0], r"^TASK-STALL run-s t1 status=running idle=\d+s$",
                         "TASK-STALL matches the documented grammar")

    def test_no_stall_when_journal_fresh(self):
        self.write_run("run-j", [("t1", "running")], age=3000)
        open(os.path.join(self.state, "run-j.journal.jsonl"), "w").write("{}\n")  # fresh mtime
        _, lines = self.run_watch()
        self.assertFalse(any(l.startswith("TASK-STALL") for l in lines),
                         "stall requires BOTH run file and journal to be old; a fresh journal means progress")

    def test_no_stall_when_run_file_fresh(self):
        self.write_run("run-f", [("t1", "running")])
        _, lines = self.run_watch()
        self.assertFalse(any(l.startswith("TASK-STALL") for l in lines),
                         "a recently modified run file is not stalled")

    def test_no_stall_for_non_running_task(self):
        self.write_run("run-p", [("t1", "pending")], age=3000)
        _, lines = self.run_watch()
        self.assertFalse(any(l.startswith("TASK-STALL") for l in lines),
                         "only tasks in `running` can stall")


class TestHeartbeat(Base):
    def test_silent_on_success_and_commands_invoked(self):
        _, lines = self.run_watch()
        self.assertFalse(any(l.startswith("HEARTBEAT") for l in lines),
                         "successful heartbeats are silent")
        c = open(self.clog).read()
        b = open(self.blog).read()
        self.assertIn("state heartbeat --run flow-%s" % SESSION, c,
                      "condukt heartbeat runs at start with the documented arguments")
        self.assertIn("driver heartbeat --session-id %s --project %s" % (SESSION, self.project), b,
                      "backlog heartbeat runs at start with the documented arguments")

    def test_failing_condukt_heartbeat(self):
        _fake(self.condukt, 3, self.clog)
        _, lines = self.run_watch()
        self.assertIn("HEARTBEAT-FAILED condukt rc=3", lines,
                      "a non-zero heartbeat exit is reported with its rc")
        self.assertFalse(any(l.startswith("HEARTBEAT-FAILED backlog") for l in lines),
                         "the healthy backlog heartbeat is not blamed")

    def test_failing_backlog_heartbeat(self):
        _fake(self.backlog, 5, self.blog)
        _, lines = self.run_watch()
        self.assertIn("HEARTBEAT-FAILED backlog rc=5", lines,
                      "a failing backlog heartbeat is reported with its rc")

    def test_missing_executable_is_failure_not_silence(self):
        env = self.env(FLOW_WATCH_CONDUKT=os.path.join(self.tmp, "nope-condukt"))
        _, lines = self.run_watch(env=env)
        self.assertIn("HEARTBEAT-FAILED condukt rc=127", lines,
                      "a missing executable is a failure with rc=127, never silence")

    def test_missing_backlog_executable(self):
        env = self.env(FLOW_WATCH_BACKLOG=os.path.join(self.tmp, "nope-backlog"))
        _, lines = self.run_watch(env=env)
        self.assertIn("HEARTBEAT-FAILED backlog rc=127", lines,
                      "a missing backlog executable is a failure with rc=127")

    def test_heartbeat_repeats_on_interval(self):
        self.run_watch(self.args(heartbeat_secs=1, max_secs=3))
        n = len(open(self.clog).read().splitlines())
        self.assertGreaterEqual(n, 2, "heartbeat runs at start and again every --heartbeat-secs")


class TestLifecycle(Base):
    def test_expired_is_last_line_exit_zero(self):
        self.write_queue([self.queued("bgr-1")])
        t0 = time.time()
        p, lines = self.run_watch()
        self.assertEqual(p.returncode, 0, "exit 0 after EXPIRED")
        self.assertEqual(lines[-1], "EXPIRED re-arm", "EXPIRED re-arm is the last stdout line")
        self.assertEqual(sum(1 for l in lines if l == "EXPIRED re-arm"), 1, "printed exactly once")
        self.assertLess(time.time() - t0, 20, "--max-secs bounds the run")

    def test_bad_args_exit_2(self):
        for bad in (["--bogus"], [], ["--session", "s"], ["--session", "s", "--project", "/p", "--poll-secs", "abc"]):
            p = subprocess.run([FLOW_WATCH] + bad, env=self.env(), capture_output=True,
                               text=True, timeout=30)
            self.assertEqual(p.returncode, 2, "bad args %r must exit 2" % (bad,))
            self.assertTrue(p.stderr.strip(), "usage goes to stderr for %r" % (bad,))
            self.assertEqual(p.stdout.strip(), "", "no event lines on stdout for bad args %r" % (bad,))

    def test_launcher_exists_and_executable(self):
        self.assertTrue(os.path.isfile(FLOW_WATCH), "crates/flow/bin/flow-watch must exist")
        self.assertTrue(os.stat(FLOW_WATCH).st_mode & stat.S_IXUSR, "launcher must be executable")


if __name__ == "__main__":
    unittest.main()
