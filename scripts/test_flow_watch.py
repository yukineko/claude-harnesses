#!/usr/bin/env python3
"""Tests for `crates/flow/bin/flow-watch` (spec: crates/flow/WATCH-SPEC.md,
including "Run tracking (v2, 2026-11-21)").

Written by an independent test author (not the implementer), RED first.

Each test runs the real launcher as a subprocess in a temp dir with fake
`condukt` / `backlog` executables (FLOW_WATCH_CONDUKT / FLOW_WATCH_BACKLOG).
The fake `condukt` answers `state list-tasks` from a JSON file the test
rewrites atomically (and an rc file), and `state heartbeat` from an rc file.
Tests wait for observable events (a stdout line, a fake-condukt log line, a
file's content) under a deadline instead of fixed sleeps.

What these tests do NOT prove:
  * Real `condukt state list-tasks` behaviour: its project scoping, its
    terminal-status semantics and its real output shape are taken from the
    spec, not observed; only the fake is exercised.
  * Timing under load / the real defaults (5s poll, 60s state, 600s heartbeat,
    1200s stall, 1700s max): all intervals here are 0.2-3 seconds.
  * Default paths (~/.blastguard/relay/<SESSION>.jsonl and the default
    --watch-state under XDG_STATE_HOME); every test passes explicit paths and
    isolates HOME/XDG_STATE_HOME to a temp dir.
  * STATE ordering/format beyond `STATE <run> <task>=<status> ...` and
    `STATE <run> finished`; exact WATCH-ERROR reason wording (only the
    `WATCH-ERROR condukt list-tasks` / `WATCH-ERROR watch-state` prefixes).
  * That a watch-state write *failure* produces WATCH-ERROR (the spec says it
    does; no test injects a write failure), nor TASK-STALL re-emission rate.
  * Terminal-status vocabulary: runs leave the watched set only by disappearing
    from the listing, which is all the spec defines.
  * Crash-safety of the state file (partial writes), SIGTERM handling, or
    concurrent watchers sharing one --watch-state.
  * Monitor-tool integration, SKILL.md wording, the version bump, or the
    git-index exec bit of the launcher.
  * Multi-session isolation, very large queues, or concurrent writers.
"""
import json
import os
import queue
import re
import shutil
import stat
import subprocess
import tempfile
import threading
import time
import unittest

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
FLOW_WATCH = os.path.join(ROOT, "crates", "flow", "bin", "flow-watch")
SESSION = "sess-test"
OTHER_SESSION = "sess-other"
OMIT = object()  # set_list: leave owner_session out of the row entirely
DEADLINE = 20


def _fake(path, rc, log):
    with open(path, "w") as f:
        f.write('#!/bin/sh\necho "$@" >> "%s"\nexit %d\n' % (log, rc))
    os.chmod(path, 0o755)


def _fake_condukt(path, log, listfile, listrc, hbrc):
    with open(path, "w") as f:
        f.write(
            '#!/bin/sh\n'
            'echo "$@" >> "%(log)s"\n'
            'case "$1 $2" in\n'
            '"state list-tasks")\n'
            '  out=$(cat "%(lf)s")\n'
            '  rc=$(cat "%(lrc)s" 2>/dev/null || echo 0)\n'
            '  echo "CWD=$(pwd -P)" >> "%(log)s"\n'
            '  echo "LISTED" >> "%(log)s"\n'
            '  printf "%%s\\n" "$out"\n'
            '  exit "$rc" ;;\n'
            '"state heartbeat")\n'
            '  exit "$(cat "%(hrc)s" 2>/dev/null || echo 0)" ;;\n'
            'esac\n'
            'exit 0\n' % {"log": log, "lf": listfile, "lrc": listrc, "hrc": hbrc})
    os.chmod(path, 0o755)


def _atomic_write(path, text):
    tmp = path + ".tmp"
    with open(tmp, "w") as f:
        f.write(text)
    os.replace(tmp, path)


class Watcher:
    """A running flow-watch whose stdout is consumed line by line."""

    def __init__(self, test, args, env):
        self.test = test
        self.lines = []
        self._q = queue.Queue()
        self.eof = False
        self.proc = subprocess.Popen(args, env=env, stdout=subprocess.PIPE,
                                     stderr=subprocess.PIPE, text=True)
        threading.Thread(target=self._pump, daemon=True).start()
        test.addCleanup(self.close)

    def _pump(self):
        for l in self.proc.stdout:
            self._q.put(l.rstrip("\n"))
        self._q.put(None)

    def _drain(self, timeout):
        try:
            l = self._q.get(timeout=timeout)
        except queue.Empty:
            return False
        if l is None:
            self.eof = True
        else:
            self.lines.append(l)
        return True

    def wait_for(self, pred, what, timeout=DEADLINE):
        """Return the first stdout line (from the start) satisfying pred."""
        end = time.time() + timeout
        i = 0
        while True:
            while i < len(self.lines):
                if pred(self.lines[i]):
                    return self.lines[i]
                i += 1
            if self.eof or time.time() > end:
                self.test.fail("never observed %s; stdout so far: %r" % (what, self.lines))
            self._drain(0.2)

    def settle(self, seconds):
        """Consume stdout for a bounded time (used only after an event-driven sync)."""
        end = time.time() + seconds
        while time.time() < end and not self.eof:
            self._drain(0.1)

    def close(self):
        if self.proc.poll() is None:
            self.proc.kill()
        self.proc.wait()
        for s in (self.proc.stdout, self.proc.stderr):
            try:
                s.close()
            except Exception:
                pass


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="flowwatch-")
        self.addCleanup(shutil.rmtree, self.tmp, True)
        self.queue = os.path.join(self.tmp, "relay.jsonl")
        self.wstate = os.path.join(self.tmp, "wstate", "watch.json")
        self.project = os.path.join(self.tmp, "proj")
        os.makedirs(self.project)
        self.clog = os.path.join(self.tmp, "condukt.log")
        self.blog = os.path.join(self.tmp, "backlog.log")
        self.condukt = os.path.join(self.tmp, "condukt")
        self.backlog = os.path.join(self.tmp, "backlog")
        self.listfile = os.path.join(self.tmp, "list.json")
        self.listrc = os.path.join(self.tmp, "list.rc")
        self.hbrc = os.path.join(self.tmp, "hb.rc")
        self.set_list([])
        _fake_condukt(self.condukt, self.clog, self.listfile, self.listrc, self.hbrc)
        _fake(self.backlog, 0, self.blog)

    # -- fake condukt controls
    def set_list(self, rows):
        """rows: iterable of (run_id, task_id, status[, owner_session[, terminal_label]]).

        owner_session defaults to SESSION (the watching session), so tests that
        predate run ownership keep exercising runs this session owns. Pass OMIT
        to leave the key out of the row; None is written as JSON null.
        terminal_label is written only when given.
        """
        out = []
        for row in rows:
            r, t, s = row[:3]
            owner = row[3] if len(row) > 3 else SESSION
            d = {"run_id": r, "task_id": t, "status": s}
            if owner is not OMIT:
                d["owner_session"] = owner
            if len(row) > 4:
                d["terminal_label"] = row[4]
            out.append(d)
        _atomic_write(self.listfile, json.dumps(out))

    def set_list_raw(self, text):
        _atomic_write(self.listfile, text)

    def set_list_rc(self, rc):
        _atomic_write(self.listrc, str(rc))

    def listed(self):
        try:
            return open(self.clog).read().splitlines().count("LISTED")
        except FileNotFoundError:
            return 0

    def wait_listed(self, n, what="fake condukt list-tasks polls"):
        """Block until the fake condukt has answered at least n list-tasks calls."""
        end = time.time() + DEADLINE
        while self.listed() < n:
            if time.time() > end:
                self.fail("timed out waiting for %s (have %d, want %d)" % (what, self.listed(), n))
            time.sleep(0.05)

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

    def args(self, extra=(), **kw):
        a = [FLOW_WATCH, "--session", SESSION, "--project", self.project,
             "--relay-queue", self.queue, "--watch-state", self.wstate,
             "--poll-secs", "0.2", "--state-secs", "0.3", "--heartbeat-secs", "600",
             "--relay-stall-secs", "120", "--task-stall-secs", "1200",
             "--max-secs", "3"]
        for k, v in kw.items():
            a[a.index("--" + k.replace("_", "-")) + 1] = str(v)
        return a + list(extra)

    def env(self, **extra):
        e = dict(os.environ)
        e["FLOW_WATCH_CONDUKT"] = self.condukt
        e["FLOW_WATCH_BACKLOG"] = self.backlog
        e["HOME"] = self.tmp
        e["XDG_STATE_HOME"] = os.path.join(self.tmp, "xdg")
        e.update(extra)
        return e

    def run_watch(self, args=None, env=None, timeout=30):
        p = subprocess.run(args or self.args(), env=env or self.env(),
                           capture_output=True, text=True, timeout=timeout)
        return p, [l for l in p.stdout.splitlines() if l.strip()]

    def watch(self, extra=(), env=None, **kw):
        kw.setdefault("max_secs", 60)
        return Watcher(self, self.args(extra=extra, **kw), env or self.env())


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
        w = self.watch()
        self.wait_listed(1, "the watcher's first poll")
        self.append_queue(self.queued("bgr-late", agent="ag2", cwd="/w/z"))
        w.wait_for(lambda l: l == "RELAY bgr-late agent=ag2 cwd=/w/z",
                   "RELAY for a queued row appended after startup (polling must pick it up)")
        self.assertIn("RELAY bgr-late agent=ag2 cwd=/w/z", w.lines,
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


class TestRunTracking(Base):
    def test_baseline_run_is_not_watched(self):
        self.set_list([("run-old", "t1", "running")])
        w = self.watch()
        self.wait_listed(1, "the first (baseline) poll")
        self.set_list([("run-new", "t1", "running"), ("run-old", "t1", "pending")])
        w.wait_for(lambda l: l.startswith("STATE run-new "),
                   "STATE for run-new, which first appeared after the baseline poll")
        self.set_list([("run-new", "t1", "pending"), ("run-old", "t1", "verifying")])
        w.wait_for(lambda l: l.startswith("STATE run-new ") and "t1=pending" in l,
                   "STATE for run-new's later change (same poll also changed run-old)")
        self.assertFalse(any("run-old" in l for l in w.lines),
                         "runs present in the first successful poll are baseline and must produce no STATE/TASK-STALL")

    def test_run_flag_watches_a_baseline_run(self):
        self.set_list([("run-old", "t1", "running"), ("run-other", "t9", "running")])
        w = self.watch(extra=["--run", "run-old"])
        line = w.wait_for(lambda l: l.startswith("STATE run-old "),
                          "STATE for a baseline run explicitly named with --run")
        self.assertIn("t1=running", line, "STATE lists the run's tasks as task=status")
        w.settle(1.0)
        self.assertFalse(any("run-other" in l for l in w.lines),
                         "--run adds only the named run; other baseline runs stay unwatched")

    def test_later_run_state_on_change_and_not_repeated(self):
        w = self.watch()
        self.wait_listed(1, "the baseline poll")
        self.set_list([("run-n", "t1", "running"), ("run-n", "t2", "pending")])
        line = w.wait_for(lambda l: l.startswith("STATE run-n "), "STATE for a run that appeared later")
        self.assertIn("t1=running", line, "STATE lists every currently listed task (t1)")
        self.assertIn("t2=pending", line, "STATE lists every currently listed task (t2)")
        self.set_list([("run-n", "t1", "running"), ("run-n", "t2", "verifying")])
        w.wait_for(lambda l: l.startswith("STATE run-n ") and "t2=verifying" in l,
                   "STATE when a watched run's (task,status) set changes")
        n = self.listed()
        self.wait_listed(n + 3, "three further unchanged polls")
        w.settle(0.5)
        states = [l for l in w.lines if l.startswith("STATE run-n ")]
        self.assertEqual(len(states), 2,
                         "STATE is emitted only when the set changes, not on every poll; got %r" % (states,))

    def test_finished_once_when_run_disappears(self):
        w = self.watch()
        self.wait_listed(1, "the baseline poll")
        self.set_list([("run-f", "t1", "running")])
        w.wait_for(lambda l: l.startswith("STATE run-f t1="), "STATE for the new run")
        self.set_list([])
        w.wait_for(lambda l: l == "STATE run-f finished",
                   "'STATE run-f finished' when a watched run vanishes from the listing")
        n = self.listed()
        self.wait_listed(n + 3, "three polls after the run finished")
        w.settle(0.5)
        self.assertEqual(sum(1 for l in w.lines if l == "STATE run-f finished"), 1,
                         "finished is printed once and the run is dropped from the watched set")

    def test_task_stall_after_unchanged_status(self):
        self.set_list([("run-old", "t1", "running")])
        w = self.watch(task_stall_secs=1)
        self.wait_listed(1, "the baseline poll")
        self.set_list([("run-s", "t1", "running"), ("run-s", "t2", "pending"),
                       ("run-old", "t1", "running")])
        line = w.wait_for(lambda l: l.startswith("TASK-STALL "),
                          "TASK-STALL after a running task's status stayed unchanged >= --task-stall-secs")
        self.assertRegex(line, r"^TASK-STALL run-s t1 status=running idle=\d+s$",
                         "TASK-STALL matches the grammar and names the running task of the watched run")
        self.assertGreaterEqual(int(re.search(r"idle=(\d+)s", line).group(1)), 1,
                                "idle is now minus first-seen and is at least --task-stall-secs")
        w.settle(0.5)
        self.assertFalse(any(l.startswith("TASK-STALL") and ("run-old" in l or " t2 " in l) for l in w.lines),
                         "no TASK-STALL for a baseline run, nor for a watched task that is not `running`")

    def test_status_change_resets_stall_clock(self):
        w = self.watch(task_stall_secs=3)
        self.wait_listed(1, "the baseline poll")
        self.set_list([("run-c", "t1", "running")])
        w.wait_for(lambda l: l.startswith("STATE run-c "), "STATE for the new run")
        t0 = time.time()
        # flip the status repeatedly: the status is never unchanged for 3s
        flip = 0
        while time.time() - t0 < 4.5:
            flip += 1
            self.set_list([("run-c", "t1", "running" if flip % 2 else "verifying")])
            time.sleep(0.5)
        w.settle(0.3)
        self.assertFalse(any(l.startswith("TASK-STALL") for l in w.lines),
                         "a status that keeps changing is progress; TASK-STALL needs the SAME status for >= --task-stall-secs")

    def test_watched_set_persists_across_restart(self):
        w1 = self.watch()
        self.wait_listed(1, "the baseline poll")
        self.set_list([("run-p", "t1", "running")])
        w1.wait_for(lambda l: l.startswith("STATE run-p "), "first process's STATE for the new run")
        end = time.time() + DEADLINE
        while True:
            try:
                if "run-p" in open(self.wstate).read():
                    break
            except FileNotFoundError:
                pass
            if time.time() > end:
                self.fail("--watch-state never recorded run-p (file at %s)" % self.wstate)
            time.sleep(0.05)
        w1.close()
        # status changes while no watcher is running
        self.set_list([("run-p", "t1", "verifying")])
        w2 = self.watch()
        line = w2.wait_for(lambda l: l.startswith("STATE run-p "),
                           "second process (same --watch-state) still reports the run the first one watched")
        self.assertIn("t1=verifying", line, "the re-armed watch reports the changed status")
        # absent control: a fresh --watch-state baselines the run away
        other = os.path.join(self.tmp, "other-wstate.json")
        n = self.listed()
        w3 = self.watch(watch_state=other)
        self.wait_listed(n + 2, "the control process's polls")
        w3.settle(0.5)
        self.assertFalse(any(l.startswith("STATE run-p") for l in w3.lines),
                         "control: without the persisted file the same run is baseline and silent")

    def test_corrupt_watch_state_is_error_and_untouched(self):
        os.makedirs(os.path.dirname(self.wstate))
        with open(self.wstate, "w") as f:
            f.write("{garbage not json")
        before = open(self.wstate, "rb").read()
        w = self.watch()
        w.wait_for(lambda l: l.startswith("WATCH-ERROR watch-state"),
                   "WATCH-ERROR watch-state for a corrupt --watch-state file")
        self.wait_listed(1, "the first poll")
        self.set_list([("run-z", "t1", "running")])
        w.wait_for(lambda l: l.startswith("STATE run-z "),
                   "watching continues with an empty set (a later run is still tracked)")
        w.settle(0.5)
        self.assertEqual(open(self.wstate, "rb").read(), before,
                         "a file that could not be parsed is never overwritten, even after a poll changed the watched set")

    @unittest.skipIf(os.geteuid() == 0, "root can read mode-000 files")
    def test_unreadable_watch_state_is_error_and_untouched(self):
        os.makedirs(os.path.dirname(self.wstate))
        with open(self.wstate, "w") as f:
            f.write('{"watched": {}}')
        before = open(self.wstate, "rb").read()
        os.chmod(self.wstate, 0)
        self.addCleanup(os.chmod, self.wstate, 0o644)
        w = self.watch()
        w.wait_for(lambda l: l.startswith("WATCH-ERROR watch-state"),
                   "WATCH-ERROR watch-state for an existing but unreadable --watch-state file")
        self.wait_listed(1, "the first poll")
        self.set_list([("run-z", "t1", "running")])
        w.wait_for(lambda l: l.startswith("STATE run-z "), "watching continues after the error")
        w.settle(0.5)
        w.close()
        os.chmod(self.wstate, 0o644)
        self.assertEqual(open(self.wstate, "rb").read(), before,
                         "an unreadable state file is left unmodified (never silently discarded)")

    def test_list_tasks_runs_with_project_as_cwd(self):
        self.run_watch()
        cwds = [l for l in open(self.clog).read().splitlines() if l.startswith("CWD=")]
        self.assertTrue(cwds, "the watcher must call `condukt state list-tasks`")
        self.assertEqual(set(cwds), {"CWD=" + os.path.realpath(self.project)},
                         "list-tasks runs with cwd = --project (condukt scopes by project)")


class TestRunOwnership(Base):
    """Run ownership (main-session ruling 2026-11-21, after the live observation in
    session 62e92d16 of STATE events for another session's parallel run
    run-20261121-040142-44869). condukt records `owner_session` per run and
    `state list-tasks` exposes it on every row. For a run that first appears
    after the baseline poll:
      * owner_session == --session            -> watched as before;
      * a different non-null owner_session    -> no STATE / TASK-STALL;
      * null / missing / unreadable owner     -> exactly one
        `WATCH-UNOWNED <run_id> label=<terminal_label>` per run, no STATE /
        TASK-STALL (an explicit unknown marker, never a silent drop);
      * a run named with --run is watched even when unowned.

    Not proven here: WATCH-UNOWNED for baseline runs (not ruled; every test
    keeps the baseline empty), the label rendering when terminal_label is
    missing/null, rows of one run that disagree on owner_session, and the
    persistence of the WATCH-UNOWNED "once" across a re-arm.
    """

    def _sync_on_own_run(self, w, rows):
        """Publish rows plus an own-session run 'run-mine', and wait until the
        watcher has demonstrably processed that poll (STATE run-mine), then a
        few more polls so any event for the other rows would have appeared."""
        self.set_list(list(rows) + [("run-mine", "t1", "running")])
        w.wait_for(lambda l: l.startswith("STATE run-mine "),
                   "STATE for the watcher's own run in the same poll (sync point)")
        n = self.listed()
        self.wait_listed(n + 4, "four further polls")
        w.settle(0.5)

    def test_other_session_run_emits_no_events(self):
        w = self.watch(task_stall_secs=1)
        self.wait_listed(1, "the baseline poll")
        self._sync_on_own_run(w, [("run-x", "t1", "running", OTHER_SESSION, "pid-44869"),
                                  ("run-x", "t2", "running", OTHER_SESSION, "pid-44869")])
        self.set_list([("run-x", "t1", "running", OTHER_SESSION, "pid-44869"),
                       ("run-x", "t2", "done", OTHER_SESSION, "pid-44869"),
                       ("run-mine", "t1", "verifying")])
        w.wait_for(lambda l: l.startswith("STATE run-mine ") and "t1=verifying" in l,
                   "STATE for the own run's change (second sync point)")
        n = self.listed()
        self.wait_listed(n + 3, "three further polls")
        w.settle(0.5)
        self.assertEqual([l for l in w.lines if "run-x" in l], [],
                         "a run owned by a DIFFERENT session must produce no STATE / TASK-STALL "
                         "(nor any other line) in this session's watcher")

    def test_same_session_run_is_watched(self):
        w = self.watch(task_stall_secs=1)
        self.wait_listed(1, "the baseline poll")
        self.set_list([("run-own", "t1", "running", SESSION, "session-x")])
        line = w.wait_for(lambda l: l.startswith("STATE run-own "),
                          "STATE for a later run owned by --session")
        self.assertIn("t1=running", line, "STATE lists the owned run's tasks")
        w.wait_for(lambda l: l.startswith("TASK-STALL run-own t1 "),
                   "TASK-STALL for the owned run's unchanged running task")
        self.assertFalse(any(l.startswith("WATCH-UNOWNED") for l in w.lines),
                         "an owned run is not unowned")

    def test_unowned_run_emits_one_watch_unowned_and_no_state(self):
        w = self.watch(task_stall_secs=1)
        self.wait_listed(1, "the baseline poll")
        unowned = [
            ("run-miss", "t1", "running", OMIT, "pid-111"),   # key missing
            ("run-miss", "t2", "pending", OMIT, "pid-111"),
            ("run-null", "t1", "running", None, "pid-222"),   # explicit null
            ("run-bad", "t1", "running", 42, "pid-333"),      # unreadable (not a string)
        ]
        self._sync_on_own_run(w, unowned)
        # a status change of the unowned runs must not turn into STATE either
        changed = [(r, t, "verifying" if s == "running" else s, o, lb) for r, t, s, o, lb in unowned]
        self.set_list(changed + [("run-mine", "t1", "verifying")])
        w.wait_for(lambda l: l.startswith("STATE run-mine ") and "t1=verifying" in l,
                   "STATE for the own run's change (second sync point)")
        n = self.listed()
        self.wait_listed(n + 3, "three further polls")
        w.settle(0.5)
        for run, label in (("run-miss", "pid-111"), ("run-null", "pid-222"), ("run-bad", "pid-333")):
            mine = [l for l in w.lines if run in l]
            self.assertEqual(mine, ["WATCH-UNOWNED %s label=%s" % (run, label)],
                             "an undetermined owner (%s) yields exactly one WATCH-UNOWNED line for the "
                             "run (not per task, not per poll) and no STATE / TASK-STALL; got %r"
                             % (run, mine))

    def test_run_flag_watches_unowned_run(self):
        # Regression guard for the fix (GREEN at base by design): the unowned
        # rule must not override an explicit --run pin. That OTHER unowned runs
        # stay unwatched is (c)'s job, not asserted here.
        w = self.watch(extra=["--run", "run-pinned"], task_stall_secs=1)
        self.wait_listed(1, "the baseline poll")
        self.set_list([("run-pinned", "t1", "running", OMIT, "pid-444")])
        line = w.wait_for(lambda l: l.startswith("STATE run-pinned "),
                          "STATE for an unowned run explicitly named with --run")
        self.assertIn("t1=running", line, "STATE lists the pinned run's tasks")
        w.wait_for(lambda l: l.startswith("TASK-STALL run-pinned t1 "),
                   "TASK-STALL for the pinned unowned run")


class TestListTasksFailures(Base):
    """A failed list-tasks poll is a WATCH-ERROR and changes nothing - never 'no tasks'."""

    def _expect_error_no_state(self, w, what):
        w.wait_for(lambda l: l.startswith("WATCH-ERROR condukt list-tasks"),
                   "WATCH-ERROR condukt list-tasks for " + what)
        n = self.listed()
        if n:
            self.wait_listed(n + 2, "polls after the error")
        w.settle(0.5)
        self.assertFalse(any(l.startswith("STATE") for l in w.lines),
                         "no STATE may be produced from a failed list-tasks poll (%s); got %r" % (what, w.lines))
        self.assertEqual(sum(1 for l in w.lines if l.startswith("WATCH-ERROR condukt list-tasks")), 1,
                         "the same reason is reported once, not on every poll (%s)" % what)

    def test_nonzero_rc_even_with_valid_json(self):
        self.set_list([("run-x", "t1", "running")])
        self.set_list_rc(3)
        w = self.watch(extra=["--run", "run-x"])
        self._expect_error_no_state(w, "a non-zero exit (stdout looked valid)")

    def test_missing_executable(self):
        env = self.env(FLOW_WATCH_CONDUKT=os.path.join(self.tmp, "nope-condukt"))
        w = self.watch(extra=["--run", "run-x"], env=env)
        self._expect_error_no_state(w, "a missing executable (rc=127)")

    def test_non_json_stdout(self):
        self.set_list_raw("this is not json")
        w = self.watch(extra=["--run", "run-x"])
        self._expect_error_no_state(w, "non-JSON stdout")

    def test_row_missing_fields(self):
        self.set_list_raw(json.dumps([{"run_id": "run-x", "task_id": "t1"}]))
        w = self.watch(extra=["--run", "run-x"])
        self._expect_error_no_state(w, "a row without a string status")

    def test_failed_poll_is_not_read_as_empty(self):
        self.set_list([("run-x", "t1", "running")])
        w = self.watch(extra=["--run", "run-x"])
        w.wait_for(lambda l: l.startswith("STATE run-x t1=running"), "STATE from a good poll")
        self.set_list_raw("garbage")
        w.wait_for(lambda l: l.startswith("WATCH-ERROR condukt list-tasks"), "the failed poll's error")
        n = self.listed()
        self.wait_listed(n + 3, "polls while the listing is broken")
        self.set_list([("run-x", "t1", "running")])
        n = self.listed()
        self.wait_listed(n + 3, "polls after recovery")
        w.settle(0.5)
        self.assertFalse(any(l == "STATE run-x finished" for l in w.lines),
                         "a failed poll must not make a watched run look finished (error is not an empty listing)")
        self.assertEqual(sum(1 for l in w.lines if l.startswith("STATE run-x t1=running")), 1,
                         "recovery to the same status is not a change")


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
        _atomic_write(self.hbrc, "3")
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
        n = sum(1 for l in open(self.clog).read().splitlines() if l.startswith("state heartbeat"))
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

    def test_condukt_state_option_is_removed(self):
        p = subprocess.run(self.args(extra=["--condukt-state", self.tmp]), env=self.env(),
                           capture_output=True, text=True, timeout=30)
        self.assertEqual(p.returncode, 2,
                         "--condukt-state was removed in v2; passing it is a bad argument (exit 2)")
        self.assertTrue(p.stderr.strip(), "usage goes to stderr")
        self.assertEqual(p.stdout.strip(), "", "no event lines on stdout for the removed option")

    def test_launcher_exists_and_executable(self):
        self.assertTrue(os.path.isfile(FLOW_WATCH), "crates/flow/bin/flow-watch must exist")
        self.assertTrue(os.stat(FLOW_WATCH).st_mode & stat.S_IXUSR, "launcher must be executable")


if __name__ == "__main__":
    unittest.main()
