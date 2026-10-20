#!/usr/bin/env python3
"""Regression test for backlog 032d28a7: tests that grab a fixed name under the
shared $TMPDIR must not flake when several `cargo test` processes run at once
(same class as f92945d3).

Observation, not a grep: each target below is a unit test that touches (or used
to touch) a FIXED path under std::env::temp_dir(). Its compiled test binary is
run as CONCURRENCY processes x RUNS iterations at once; every iteration must pass.

Targets (the sites the item's sweep found):
  * fugu-router store::tests::import_episodes_* — used fixed dirs
    ("fugu-router-import-ep-test", ...); fixed by f502dccd (unique per-process,
    per-call dirs). The prior auditor measured 34-40/60 failures per process with
    4 concurrent processes at e70d48dd.
  * blastguard rule_id ca_blastguard_010_* — still writes the fixed
    "blastguard-rule-id-unrecoverable.txt".
  * harness-status today_does_not_create_world_writable_tmp_file — still removes
    the fixed ".harness-status-date".

The binaries are built with `cargo test --no-run`. Set BACKLOG_032D28A7_BIN_<CRATE>
(CRATE upper-cased, '-' -> '_') to an explicit test executable to run the same
stress against another build (that is how RED was shown against f502dccd^).
"""

import json
import os
import subprocess
import threading
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
CONCURRENCY = int(os.environ.get("BACKLOG_032D28A7_CONCURRENCY", "4"))
RUNS = int(os.environ.get("BACKLOG_032D28A7_RUNS", "30"))

TARGETS = [
    ("fugu-router", "store::tests::import_episodes"),
    ("blastguard", "rule_id::tests::ca_blastguard_010_interpreter_deny_reasons_map_to_stable_rule_id"),
    ("harness-status", "today_does_not_create_world_writable_tmp_file"),
]


def _cargo():
    home_cargo = Path.home() / ".cargo" / "bin" / "cargo"
    return str(home_cargo) if home_cargo.exists() else "cargo"


def unit_test_exe(crate):
    override = os.environ.get("BACKLOG_032D28A7_BIN_" + crate.upper().replace("-", "_"))
    if override:
        return override
    p = subprocess.run(
        [_cargo(), "test", "-p", crate, "--no-run", "--message-format=json"],
        cwd=REPO, capture_output=True, text=True, timeout=1800,
    )
    if p.returncode != 0:
        raise AssertionError("cargo test --no-run -p %s failed:\n%s" % (crate, p.stderr[-3000:]))
    exes = []
    for line in p.stdout.splitlines():
        try:
            d = json.loads(line)
        except ValueError:
            continue
        if d.get("reason") == "compiler-artifact" and d.get("executable") and d.get("profile", {}).get("test"):
            if d["target"]["kind"] in (["lib"], ["bin"]):
                exes.append(d["executable"])
    if not exes:
        raise AssertionError("no unit-test executable for %s" % crate)
    return exes


def stress(exes, filt):
    if isinstance(exes, str):
        exes = [exes]
    # pick the executable(s) that actually contain the filter
    chosen = []
    for e in exes:
        lst = subprocess.run([e, "--list", filt], capture_output=True, text=True, timeout=120)
        if ": test" in lst.stdout:
            chosen.append(e)
    if not chosen:
        raise AssertionError("no executable lists a test matching %r" % filt)
    fails = [0] * CONCURRENCY
    samples = []

    def worker(i):
        for _ in range(RUNS):
            for e in chosen:
                r = subprocess.run([e, filt, "--test-threads=1", "-q"], capture_output=True, text=True, timeout=300)
                if r.returncode != 0:
                    fails[i] += 1
                    if len(samples) < 2:
                        samples.append((r.stdout + r.stderr)[-1500:])

    ts = [threading.Thread(target=worker, args=(i,)) for i in range(CONCURRENCY)]
    for t in ts:
        t.start()
    for t in ts:
        t.join()
    return fails, samples


class SharedTmpConcurrency(unittest.TestCase):
    def _check(self, crate, filt):
        fails, samples = stress(unit_test_exe(crate), filt)
        self.assertEqual(
            sum(fails), 0,
            "%s %s: failures per process with %d concurrent processes x %d runs: %s\n%s"
            % (crate, filt, CONCURRENCY, RUNS, fails, "\n---\n".join(samples)),
        )

    def test_fugu_router_store_import(self):
        self._check(*TARGETS[0])

    def test_blastguard_rule_id_fixed_probe_file(self):
        self._check(*TARGETS[1])

    def test_harness_status_legacy_date_file(self):
        self._check(*TARGETS[2])


if __name__ == "__main__":
    unittest.main()
