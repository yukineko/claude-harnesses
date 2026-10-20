#!/usr/bin/env python3
"""Closure regression for backlog 8696dd7e (fault injection as a gate).

The item's four completion conditions, each pinned here:
  1. boundary has a cfg-feature thread-local FaultPlan able to force all four IO
     entrances to Undetermined;
  2. degrade exposes assert_fails_closed;
     (1 and 2 are pinned by running harness-core's tests/fault_plan.rs, which
     includes blind_faults_all_four_entries_when_actually_exercised and the
     assert_fails_closed_* tests, and requiring it green with none filtered out)
  3. at least one gate crate adopts the seam: every crate the ratchet counts has
     a test that actually calls assert_fails_closed / with_fault_plan, and the
     committed baseline is >= 1;
  4. the adoption ratchet is in the pre-commit `run` list, and it exits 0 at
     HEAD and 1 when one adopter is dropped (checked on a copy of the manifests).

Needs cargo: CARGO env, else $HOME/.cargo/bin/cargo. A missing cargo FAILS
(not skips) -- not being able to run the suite is not a pass.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent
SCRIPT = REPO / "scripts" / "check-fault-injection-adoption.py"
BASELINE = REPO / "scripts" / "check-fault-injection-adoption.baseline"


def _adopters() -> list[str]:
    p = subprocess.run(["python3", str(SCRIPT), "--list"], capture_output=True, text=True, cwd=REPO)
    names = []
    for line in p.stdout.splitlines():
        m = re.search(r"\b([a-z0-9-]+)\b", line.strip())
        if m and (REPO / "crates" / m.group(1) / "Cargo.toml").is_file() and m.group(1) != "harness-core":
            names.append(m.group(1))
    return sorted(set(names))


class Backlog8696dd7e(unittest.TestCase):
    def test_ratchet_is_wired_into_precommit_run_list(self):
        text = (REPO / ".githooks" / "pre-commit").read_text(encoding="utf-8")
        self.assertRegex(text, r"(?m)^run check-fault-injection-adoption\.py\s+\S+")

    def test_ratchet_passes_at_head_with_a_nonzero_baseline(self):
        p = subprocess.run(["python3", str(SCRIPT)], capture_output=True, text=True, cwd=REPO)
        self.assertEqual(p.returncode, 0, p.stdout + p.stderr)
        self.assertGreaterEqual(int(BASELINE.read_text().strip()), 1)

    def test_every_counted_adopter_actually_exercises_the_seam(self):
        adopters = _adopters()
        self.assertEqual(len(adopters), int(BASELINE.read_text().strip()), adopters)
        for c in adopters:
            hits = [
                f for f in (REPO / "crates" / c).rglob("*.rs")
                if "target" not in f.parts
                and re.search(r"assert_fails_closed|with_fault_plan", f.read_text(encoding="utf-8", errors="replace"))
            ]
            self.assertTrue(hits, f"{c} enables fault-injection but no test uses it")

    def test_dropping_one_adopter_makes_the_ratchet_exit_1(self):
        adopters = _adopters()
        self.assertTrue(adopters)
        tmp = Path(tempfile.mkdtemp(prefix="bl-8696dd7e."))
        self.addCleanup(lambda: shutil.rmtree(tmp, ignore_errors=True))
        (tmp / "scripts").mkdir()
        shutil.copy(SCRIPT, tmp / "scripts" / SCRIPT.name)
        shutil.copy(BASELINE, tmp / "scripts" / BASELINE.name)
        shutil.copy(REPO / "Cargo.toml", tmp / "Cargo.toml")
        for d in (REPO / "crates").iterdir():
            if (d / "Cargo.toml").is_file():
                (tmp / "crates" / d.name).mkdir(parents=True)
                shutil.copy(d / "Cargo.toml", tmp / "crates" / d.name / "Cargo.toml")
            elif d.is_dir():
                shutil.copytree(d, tmp / "crates" / d.name, ignore=shutil.ignore_patterns("target"))
        run = lambda: subprocess.run(["python3", str(tmp / "scripts" / SCRIPT.name)], capture_output=True, text=True, cwd=tmp)
        control = run()
        self.assertEqual(control.returncode, 0, control.stdout + control.stderr)
        victim = tmp / "crates" / adopters[0] / "Cargo.toml"
        victim.write_text(victim.read_text().replace('"fault-injection"', '"not-the-feature"'))
        p = run()
        self.assertEqual(p.returncode, 1, p.stdout + p.stderr)

    def test_harness_core_fault_plan_suite_is_green(self):
        cargo = os.environ.get("CARGO") or str(Path.home() / ".cargo" / "bin" / "cargo")
        self.assertTrue(Path(cargo).is_file(), f"cargo not found at {cargo}")
        p = subprocess.run(
            [cargo, "test", "-p", "harness-core", "--features", "fault-injection", "--test", "fault_plan"],
            capture_output=True, text=True, cwd=REPO,
        )
        out = p.stdout + p.stderr
        self.assertEqual(p.returncode, 0, out[-3000:])
        for name in (
            "blind_faults_all_four_entries_when_actually_exercised",
            "assert_fails_closed_panics_on_a_gate_that_maps_undetermined_to_clean",
            "assert_fails_closed_passes_and_returns_the_value_for_a_correct_gate",
        ):
            self.assertRegex(out, rf"test {name} \.\.\. ok")
        m = re.search(r"test result: ok\. (\d+) passed; 0 failed; (\d+) ignored; \d+ measured; (\d+) filtered out", out)
        self.assertIsNotNone(m, out[-2000:])
        self.assertGreaterEqual(int(m.group(1)), 27)
        self.assertEqual(m.group(3), "0")


if __name__ == "__main__":
    unittest.main()
