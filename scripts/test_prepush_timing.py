#!/usr/bin/env python3
"""Tests for the per-phase timing and the bounded target-dir cleanup that
.githooks/pre-push gained in d60e62c7 and 7511bf6d.

Run: `python3 scripts/test_prepush_timing.py` (stdlib `unittest` only).

Reuses `HookHarness` from scripts/test_prepush_hook.py (a throwaway git repo
holding a copy of the hook, stub gate-bypass / cargo / check-compile-fail, and a
PATH made of symlinks). Set PREPUSH_HOOK_UNDER_TEST to point the suite at
another copy of the hook (the mutation runs do exactly that).

What is pinned
--------------
1. Timing never changes the exit code (allow, cargo fail, compile-fail rc 1 and
   rc 2, missing compile-fail script, bypass block). Each scenario's rc is
   compared with a literal AND with the hook as it was before d60e62c7.
2. One `pre-push timing: ... total Ns (exit RC)` summary line on stderr, on
   allow and on block; on a block it lists the phases up to the blocking one.
3. Exactly one JSON line per run is appended to <git-common-dir>/
   prepush-timing.jsonl; `exit` equals the hook rc, `shas` the pushed shas.
4. An unwritable log (a directory in its place) gives one warning, same rc.
5. compile-fail stderr passes through line by line; an rc that never comes back
   blocks.
6. target-cleanup: threshold, env fallback, unreadable dir, live/dead sibling
   pid, held flock, no rc change, and an anti-vacuity pair on one fixture.

A test that only ever sees the "kept" outcome proves nothing about the delete
path, so the delete tests assert the directory is really gone, and every
"no delete" test has a sibling control that is the same fixture without the one
knob and does delete.
"""

from __future__ import annotations

import fcntl
import json
import os
import re
import subprocess
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent))

import test_prepush_hook as tph  # noqa: E402
from test_prepush_hook import HookHarness, HookTestCase, _ZERO_SHA, _which  # noqa: E402

_REPO = Path(__file__).resolve().parent.parent
_PRE_TIMING_COMMIT = "d60e62c7^"  # the hook before timing was added

_SUMMARY_RE = re.compile(
    r"^pre-push timing: (?P<phases>.*); total (?P<total>\d+|\?)s \(exit (?P<rc>\d+)\)$",
    re.MULTILINE,
)


class TimedHarness(HookHarness):
    """HookHarness plus `date` and `rm` on the hook's PATH (the hook's timing
    and target removal use them; the base harness PATH has neither)."""

    def __init__(self, **kw):
        super().__init__(**kw)
        for tool in (_which("date"), _which("rm")):
            link = self.bindir / Path(tool).name
            if not link.exists():
                os.symlink(tool, link)
        self.git_common = self.root / ".git"
        self.log = self.git_common / "prepush-timing.jsonl"
        self.target = self.git_common / "prepush-typecheck-target"
        self.deps = self.target / "debug" / "deps"

    def _git(self, *args):
        subprocess.run(
            [_which("git"), "-C", str(self.root), *args],
            check=True,
            capture_output=True,
        )

    def _amend(self):
        self._git("add", "-A")
        self._git("commit", "-q", "--amend", "--no-edit")
        self.local_sha = subprocess.run(
            [_which("git"), "-C", str(self.root), "rev-parse", "HEAD"],
            check=True,
            capture_output=True,
            text=True,
        ).stdout.strip()

    def set_compile_fail(self, body):
        p = self.root / "scripts" / "check-compile-fail.py"
        p.write_text(body)
        p.chmod(0o755)
        self._amend()

    def remove_compile_fail(self):
        (self.root / "scripts" / "check-compile-fail.py").unlink()
        self._amend()

    def run_with(self, env_extra=None, stdin=None):
        env = dict(self.env)
        env.update(env_extra or {})
        if stdin is None:
            stdin = "refs/heads/main %s refs/heads/main %s\n" % (self.local_sha, _ZERO_SHA)
        return subprocess.run(
            [_which("sh"), str(self.hook), "origin", "https://example.invalid/repo.git"],
            cwd=str(self.root),
            env=env,
            input=stdin,
            capture_output=True,
            text=True,
        )

    def log_lines(self):
        if not self.log.is_file():
            return []
        return [ln for ln in self.log.read_text().split("\n") if ln != ""]

    def make_deps(self, n):
        self.deps.mkdir(parents=True)
        for i in range(n):
            (self.deps / ("artifact-%03d" % i)).write_text("x")


def _cf_script(rc, lines=()):
    out = "".join("sys.stderr.write(%r)\n" % ln for ln in lines)
    return "#!/usr/bin/env python3\nimport sys\n%ssys.exit(%d)\n" % (out, rc)


class TimingTestCase(HookTestCase):
    def harness(self, **kwargs):
        h = TimedHarness(**kwargs)
        self.addCleanup(h.cleanup)
        return h

    def summary(self, proc):
        found = list(_SUMMARY_RE.finditer(proc.stderr))
        self.assertEqual(
            len(found), 1,
            "stderr must carry exactly one timing summary line\n--- stderr ---\n" + proc.stderr,
        )
        return found[0]

    def phase_entry(self, h, name, line=-1):
        rec = json.loads(h.log_lines()[line])
        hits = [p for p in rec["phases"] if p["phase"] == name]
        self.assertEqual(len(hits), 1, "phase %r not exactly once in %r" % (name, rec["phases"]))
        return hits[0]


# --- scenarios: (name, harness kwargs, mutator, expected rc, phases in, phases out)
def _cf_rc1(h):
    h.set_compile_fail(_cf_script(1, ["check-compile-fail: FAILED boom\n"]))


def _cf_rc2(h):
    h.set_compile_fail(_cf_script(2, ["check-compile-fail: UNDETERMINED x\n"]))


def _cf_missing(h):
    h.remove_compile_fail()


_SCENARIOS = [
    ("allow", {}, None, 0,
     ["bypass-ledger", "nudges", "target-cleanup", "typecheck", "compile-fail", "cleanup", "ref-recheck"], []),
    ("cargo-fail", {"cargo_exit": 1}, None, 1,
     ["bypass-ledger", "nudges", "target-cleanup", "typecheck"], ["compile-fail", "cleanup", "ref-recheck"]),
    ("compile-fail-rc1", {}, _cf_rc1, 1,
     ["bypass-ledger", "nudges", "target-cleanup", "typecheck", "compile-fail"], ["cleanup", "ref-recheck"]),
    ("compile-fail-rc2", {}, _cf_rc2, 1,
     ["bypass-ledger", "nudges", "target-cleanup", "typecheck", "compile-fail"], ["cleanup", "ref-recheck"]),
    ("compile-fail-missing", {}, _cf_missing, 1,
     ["bypass-ledger", "nudges", "target-cleanup", "typecheck", "compile-fail"], ["cleanup", "ref-recheck"]),
    ("bypass-block", {"gate_bypass_exit": 1}, None, 1,
     ["bypass-ledger"], ["nudges", "target-cleanup", "typecheck", "ref-recheck"]),
]


def _baseline_hook_text():
    proc = subprocess.run(
        ["git", "-C", str(_REPO), "show", _PRE_TIMING_COMMIT + ":.githooks/pre-push"],
        capture_output=True,
        text=True,
    )
    if proc.returncode != 0:
        raise AssertionError("cannot read the pre-timing hook: " + proc.stderr)
    return proc.stdout


class ExitCodeUnchangedByTiming(TimingTestCase):
    """(1) rc is exactly what it was; compared with a literal and with the hook
    as of d60e62c7^."""

    def _prepare(self, kwargs, mutator):
        h = self.harness(**kwargs)
        if mutator:
            mutator(h)
        return h

    def test_scenarios(self):
        baseline = _baseline_hook_text()
        for name, kwargs, mutator, rc, _, _ in _SCENARIOS:
            with self.subTest(name):
                h = self._prepare(kwargs, mutator)
                got = h.run_with().returncode
                self.assertEqual(got, rc, name + ": hook rc")
                hb = self._prepare(kwargs, mutator)
                hb.hook.write_text(baseline)
                self.assertEqual(
                    hb.run_with().returncode, got,
                    name + ": rc differs from the pre-timing hook",
                )

    def test_allow_is_really_allow_and_blocks_are_real(self):
        # anti-vacuity: the six scenarios are not all the same outcome.
        rcs = set()
        for _, kwargs, mutator, _, _, _ in _SCENARIOS:
            h = self._prepare(kwargs, mutator)
            rcs.add(h.run_with().returncode)
        self.assertEqual(rcs, {0, 1})


class SummaryLine(TimingTestCase):
    """(2) one summary line, on allow and on block, listing phases up to the
    blocking one."""

    def test_summary_per_scenario(self):
        for name, kwargs, mutator, rc, present, absent in _SCENARIOS:
            with self.subTest(name):
                h = self.harness(**kwargs)
                if mutator:
                    mutator(h)
                proc = h.run_with()
                m = self.summary(proc)
                self.assertEqual(int(m.group("rc")), rc)
                self.assertEqual(proc.returncode, rc)
                self.assertRegex(m.group("total"), r"^\d+$", "date is on PATH: total must be measured")
                names = re.findall(
                    r"(?:^|, )([a-z-]+)(?:\([0-9a-f]{8}\))? (?:\d+|\?)s", m.group("phases")
                )
                self.assertEqual(names, present, "listed phases %r" % m.group("phases"))
                for no in absent:
                    self.assertNotIn(no, names)

    def test_summary_is_last_stderr_line_and_one_per_run(self):
        h = self.harness()
        proc = h.run_with()
        self.assertTrue(_SUMMARY_RE.match(proc.stderr.rstrip("\n").split("\n")[-1]))


class JsonLog(TimingTestCase):
    """(3) exactly one JSON line per run."""

    def test_one_line_per_run_allow_and_block(self):
        h = self.harness()
        p1 = h.run_with()
        self.assertEqual(len(h.log_lines()), 1)
        p2 = h.run_with()
        self.assertEqual(len(h.log_lines()), 2)
        h.set_compile_fail(_cf_script(1))
        p3 = h.run_with()
        lines = h.log_lines()
        self.assertEqual(len(lines), 3)
        recs = [json.loads(ln) for ln in lines]
        self.assertEqual([r["exit"] for r in recs], [p1.returncode, p2.returncode, p3.returncode])
        self.assertEqual([p1.returncode, p2.returncode, p3.returncode], [0, 0, 1])

    def test_fields(self):
        h = self.harness()
        proc = h.run_with()
        rec = json.loads(h.log_lines()[0])
        self.assertEqual(rec["exit"], proc.returncode)
        self.assertEqual(rec["shas"], [h.local_sha])
        self.assertIsInstance(rec["total"], int)
        self.assertRegex(rec["start"], r"^\d{4}-\d\d-\d\dT\d\d:\d\d:\d\dZ$")
        names = [p["phase"] for p in rec["phases"]]
        self.assertEqual(
            names,
            ["bypass-ledger", "nudges", "target-cleanup", "typecheck", "compile-fail", "cleanup", "ref-recheck"],
        )

    def test_block_record_has_hook_rc(self):
        h = self.harness(cargo_exit=1)
        proc = h.run_with()
        rec = json.loads(h.log_lines()[0])
        self.assertEqual(proc.returncode, 1)
        self.assertEqual(rec["exit"], 1)
        self.assertEqual(rec["shas"], [h.local_sha])
        self.assertEqual(rec["phases"][-1]["phase"], "typecheck")

    def test_two_pushed_shas(self):
        h = self.harness()
        sha1 = h.local_sha
        h._git("branch", "first", sha1)
        (h.root / "g.txt").write_text("2\n")
        h._git("add", "-A")
        h._git("commit", "-q", "-m", "second")
        sha2 = subprocess.run(
            [_which("git"), "-C", str(h.root), "rev-parse", "HEAD"],
            capture_output=True, text=True, check=True,
        ).stdout.strip()
        stdin = "refs/heads/first %s refs/heads/first %s\nrefs/heads/main %s refs/heads/main %s\n" % (
            sha1, _ZERO_SHA, sha2, _ZERO_SHA)
        proc = h.run_with(stdin=stdin)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        rec = json.loads(h.log_lines()[0])
        self.assertEqual(rec["shas"], [sha1, sha2])
        self.assertEqual(rec["exit"], 0)


class UnwritableLog(TimingTestCase):
    """(4) a log that cannot be written: one warning, same rc."""

    def test_allow_and_block(self):
        for name, kwargs, expected in (("allow", {}, 0), ("block", {"cargo_exit": 1}, 1)):
            with self.subTest(name):
                h = self.harness(**kwargs)
                h.log.mkdir()
                proc = h.run_with()
                self.assertEqual(proc.returncode, expected, proc.stderr)
                warns = [ln for ln in proc.stderr.split("\n") if "could not append to" in ln]
                self.assertEqual(len(warns), 1, proc.stderr)
                self.assertIn("prepush-timing.jsonl", warns[0])
                self.summary(proc)

    def test_control_writable_log_has_no_warning(self):
        h = self.harness()
        proc = h.run_with()
        self.assertNotIn("could not append", proc.stderr)


class CompileFailPassThrough(TimingTestCase):
    """(5) stderr of check-compile-fail.py comes through unchanged, line by
    line; an rc that never comes back blocks."""

    LINES = [
        "check-compile-fail: running alpha ...\n",
        "check-compile-fail: ok alpha\n",
        "  leading spaces and a \\backslash and $HOME and `tick` and 'q' \"dq\"\n",
        "\n",
        "check-compile-fail: running beta ...\n",
        "check-compile-fail: FAILED beta\n",
    ]

    def test_lines_in_order_and_unchanged(self):
        h = self.harness()
        h.set_compile_fail(_cf_script(0, self.LINES + ["last line without newline"]))
        proc = h.run_with()
        self.assertEqual(proc.returncode, 0, proc.stderr)
        got = proc.stderr.split("\n")
        want = [ln.rstrip("\n") for ln in self.LINES] + ["last line without newline"]
        pos = -1
        for w in want:
            try:
                pos = got.index(w, pos + 1)
            except ValueError:
                self.fail("line %r not passed through in order\n--- stderr ---\n%s" % (w, proc.stderr))

    def test_per_target_breakdown_recorded(self):
        h = self.harness()
        h.set_compile_fail(_cf_script(0, self.LINES))
        h.run_with()
        entry = self.phase_entry(h, "compile-fail")
        self.assertEqual(sorted(entry["targets"]), ["alpha", "beta"])

    def test_rc_that_never_comes_back_blocks(self):
        mark = None
        h = self.harness()
        mark = h.root / "killed.mark"
        h.set_compile_fail(
            "#!/usr/bin/env python3\n"
            "import os, signal, subprocess, sys\n"
            "hook_pid = int(os.getcwd().rsplit('.', 1)[1])\n"
            "chain = []\n"
            "p = os.getppid()\n"
            "while p != hook_pid and p > 1:\n"
            "    chain.append(p)\n"
            "    p = int(subprocess.run(['/bin/ps', '-o', 'ppid=', '-p', str(p)],\n"
            "        capture_output=True, text=True).stdout.strip())\n"
            "victims = chain[:-1]\n"
            "open(os.environ['CF_MARK'], 'w').write('killed:%d' % len(victims))\n"
            "for v in reversed(victims):\n"
            "    os.kill(v, signal.SIGKILL)\n"
            "sys.exit(0)\n"
        )
        proc = h.run_with({"CF_MARK": str(mark)})
        self.assertTrue(mark.is_file(), "stub never ran its kill step\n" + proc.stderr)
        self.assertNotEqual(mark.read_text(), "killed:0", "no process was killed; test setup is vacuous")
        self.assertEqual(proc.returncode, 1, "a missing rc must block\n" + proc.stderr)
        self.assertIn("BLOCKED", proc.stderr)
        self.assertEqual(json.loads(h.log_lines()[0])["exit"], 1)


class TargetCleanup(TimingTestCase):
    """(6) bounded cleanup of $GIT_COMMON/prepush-typecheck-target."""

    N = 5
    MAX = {"PREPUSH_TARGET_MAX_DEPS": "3"}

    def fixture(self, **kw):
        h = self.harness(**kw)
        h.make_deps(self.N)
        return h

    def action(self, h):
        return self.phase_entry(h, "target-cleanup")["action"]

    def test_over_threshold_removes(self):
        h = self.fixture()
        proc = h.run_with(self.MAX)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertFalse(h.target.exists(), "target dir must be removed")
        e = self.phase_entry(h, "target-cleanup")
        self.assertEqual(e["action"], "removed")
        self.assertEqual(e["threshold"], 3)
        self.assertEqual(e["deps_entries_seen"], 4)  # stops at threshold+1
        self.assertIn("target-cleanup", self.summary(proc).group("phases"))

    def test_exactly_threshold_keeps(self):
        h = self.harness()
        h.make_deps(3)
        proc = h.run_with(self.MAX)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(h.deps.is_dir())
        self.assertEqual(len(list(h.deps.iterdir())), 3)
        e = self.phase_entry(h, "target-cleanup")
        self.assertEqual((e["action"], e["deps_entries_seen"]), ("kept", 3))

    def test_one_over_threshold_removes(self):
        h = self.harness()
        h.make_deps(4)
        h.run_with(self.MAX)
        self.assertFalse(h.target.exists())

    def test_invalid_env_falls_back_to_default(self):
        for bad in ("abc", "-1", "1.5", "3x"):
            with self.subTest(bad):
                h = self.fixture()
                proc = h.run_with({"PREPUSH_TARGET_MAX_DEPS": bad})
                self.assertEqual(proc.returncode, 0, proc.stderr)
                self.assertIn("using 20000", proc.stderr)
                self.assertTrue(h.deps.is_dir(), "5 entries is far below the default 20000")
                e = self.phase_entry(h, "target-cleanup")
                self.assertEqual((e["action"], e["threshold"], e["deps_entries_seen"]), ("kept", 20000, 5))

    def test_unreadable_deps_does_not_delete(self):
        h = self.fixture()
        os.chmod(h.deps, 0)
        self.addCleanup(os.chmod, h.deps, 0o755)
        proc = h.run_with(self.MAX)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(h.target.exists())
        self.assertEqual(self.action(h), "skipped-undetermined")
        self.assertIn("could not count", proc.stderr)

    def test_live_sibling_pid_does_not_delete_and_dead_pid_does(self):
        # live: this very process
        h = self.fixture()
        (h.git_common / ("prepush-typecheck.%d" % os.getpid())).mkdir()
        proc = h.run_with(self.MAX)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(h.target.exists(), "live sibling: must not delete")
        self.assertEqual(self.action(h), "skipped-busy")
        self.assertIn("another pre-push (pid %d)" % os.getpid(), proc.stderr)
        # dead: a child that has exited and been reaped
        child = subprocess.Popen([sys.executable, "-c", "pass"])
        child.wait()
        h2 = self.fixture()
        (h2.git_common / ("prepush-typecheck.%d" % child.pid)).mkdir()
        proc2 = h2.run_with(self.MAX)
        self.assertEqual(proc2.returncode, 0, proc2.stderr)
        self.assertFalse(h2.target.exists(), "dead sibling pid: must delete")
        self.assertEqual(self.action(h2), "removed")

    def test_held_cargo_lock_does_not_delete_and_released_does(self):
        h = self.fixture()
        lockfile = h.target / "debug" / ".cargo-lock"
        lockfile.write_text("")
        fd = os.open(lockfile, os.O_RDONLY)
        try:
            fcntl.flock(fd, fcntl.LOCK_EX)
            proc = h.run_with(self.MAX)
        finally:
            os.close(fd)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertTrue(h.target.exists(), "held flock: must not delete")
        self.assertEqual(self.action(h), "skipped-busy")
        self.assertIn("holds a lock", proc.stderr)
        # control: same fixture, lock released
        proc2 = h.run_with(self.MAX)
        self.assertEqual(proc2.returncode, 0, proc2.stderr)
        self.assertFalse(h.target.exists(), "released flock: must delete")

    def test_no_branch_changes_rc(self):
        def live(h):
            (h.git_common / ("prepush-typecheck.%d" % os.getpid())).mkdir()

        def unreadable(h):
            os.chmod(h.deps, 0)
            self.addCleanup(os.chmod, h.deps, 0o755)

        branches = {"removed": None, "skipped-busy": live, "skipped-undetermined": unreadable}
        for name, prep in branches.items():
            for kw, want in (({}, 0), ({"cargo_exit": 1}, 1)):
                with self.subTest("%s cargo_exit=%s" % (name, kw.get("cargo_exit", 0))):
                    h = self.fixture(**kw)
                    if prep:
                        prep(h)
                    proc = h.run_with(self.MAX)
                    self.assertEqual(proc.returncode, want, proc.stderr)
                    self.assertEqual(self.action(h), name)
                    self.assertEqual(json.loads(h.log_lines()[0])["exit"], want)
        h = self.harness()
        h.make_deps(2)
        self.assertEqual(h.run_with(self.MAX).returncode, 0)  # kept

    def test_anti_vacuity_pair_same_fixture(self):
        """Same fixture, one knob: without it the dir is removed, with it the
        dir (and a sentinel inside it) survives. Neither outcome can be a
        constant."""
        def build():
            h = self.fixture()
            (h.target / "debug" / "sentinel").write_text("keep me")
            return h

        removed = build()
        removed.run_with(self.MAX)
        skipped = build()
        (skipped.git_common / ("prepush-typecheck.%d" % os.getpid())).mkdir()
        skipped.run_with(self.MAX)
        self.assertFalse((removed.target / "debug" / "sentinel").exists())
        self.assertTrue((skipped.target / "debug" / "sentinel").exists())
        self.assertEqual(self.action(removed), "removed")
        self.assertEqual(self.action(skipped), "skipped-busy")


if __name__ == "__main__":
    unittest.main()
