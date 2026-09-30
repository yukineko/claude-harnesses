#!/usr/bin/env python3
"""Behavioural tests for scripts/check-fault-injection-adoption.py (backlog 8696dd7e, slice 3).

The ratchet counts crates that ADOPT the harness-core `fault-injection` seam and
fails when that count drops below a committed baseline.

Contract pinned here (the implementer must satisfy it; it is not in the item text verbatim):
  * The script derives its repo root from its OWN location (REPO = parent of scripts/),
    like check-raw-io-ratchet.py, so a fixture repo is built by copying the script
    into <fixture>/scripts/ and the baseline lives at
    <fixture>/scripts/check-fault-injection-adoption.baseline (a single integer line).
  * A crate ADOPTS iff crates/<c>/Cargo.toml lists harness-core in [dev-dependencies]
    with features containing "fault-injection". Mentions in comments or in other
    tables do not count. (harness-core itself is not in any fixture.)
  * count < baseline            -> exit non-zero  (adoption regressed)
  * count == baseline           -> exit 0
  * cannot count / bad baseline -> exit non-zero  (never a silent pass)
  * "fault-injection" enabled under [dependencies] (ships in the binary) -> exit non-zero
    even when count == baseline.
Above-baseline behaviour is deliberately NOT pinned (design choice: re-pin vs allow).

Run: python3 scripts/test_check_fault_injection_adoption.py
"""
from __future__ import annotations

import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
SCRIPT_NAME = "check-fault-injection-adoption.py"
SCRIPT = _HERE / SCRIPT_NAME
BASELINE_NAME = "check-fault-injection-adoption.baseline"

ADOPTER = """[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
harness-core = {{ path = "../harness-core" }}

[dev-dependencies]
harness-core = {{ path = "../harness-core", features = ["fault-injection"] }}
"""

NON_ADOPTER = """[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
harness-core = {{ path = "../harness-core" }}
"""

COMMENT_ONLY = NON_ADOPTER + "\n# TODO adopt features = [\"fault-injection\"] in dev-dependencies\n"

SHIPPED = """[package]
name = "{name}"
version = "0.1.0"
edition = "2021"

[dependencies]
harness-core = {{ path = "../harness-core", features = ["fault-injection"] }}

[dev-dependencies]
harness-core = {{ path = "../harness-core", features = ["fault-injection"] }}
"""


class Fixture:
    def __init__(self, crates: dict[str, str], baseline: str | None):
        self.tmp = Path(tempfile.mkdtemp(prefix="fi-adopt-"))
        (self.tmp / "scripts").mkdir()
        shutil.copy(SCRIPT, self.tmp / "scripts" / SCRIPT_NAME) if SCRIPT.exists() else None
        for name, body in crates.items():
            d = self.tmp / "crates" / name
            (d / "src").mkdir(parents=True)
            (d / "src" / "lib.rs").write_text("")
            (d / "Cargo.toml").write_text(body.format(name=name))
        if baseline is not None:
            (self.tmp / "scripts" / BASELINE_NAME).write_text(baseline)
        subprocess.run(["git", "init", "-q"], cwd=self.tmp, check=True)
        subprocess.run(["git", "add", "."], cwd=self.tmp, check=True)

    def run(self) -> subprocess.CompletedProcess:
        # A missing script would make every "exits non-zero" test pass vacuously.
        assert (self.tmp / "scripts" / SCRIPT_NAME).exists(), f"{SCRIPT} does not exist (RED)"
        return subprocess.run(
            [sys.executable, str(self.tmp / "scripts" / SCRIPT_NAME)],
            cwd=self.tmp, capture_output=True, text=True, timeout=60,
        )

    def close(self):
        shutil.rmtree(self.tmp, ignore_errors=True)


class Base(unittest.TestCase):
    def fx(self, crates, baseline):
        f = Fixture(crates, baseline)
        self.addCleanup(f.close)
        return f

    def assertNonZero(self, r):
        self.assertNotEqual(r.returncode, 0, f"stdout={r.stdout!r} stderr={r.stderr!r}")

    def assertZero(self, r):
        self.assertEqual(r.returncode, 0, f"stdout={r.stdout!r} stderr={r.stderr!r}")


class Ratchet(Base):
    def test_script_exists(self):
        self.assertTrue(SCRIPT.exists(), f"{SCRIPT} does not exist")

    def test_count_equal_to_baseline_passes(self):
        r = self.fx({"a": ADOPTER, "b": ADOPTER, "c": NON_ADOPTER}, "2\n").run()
        self.assertZero(r)

    def test_count_below_baseline_fails(self):
        r = self.fx({"a": ADOPTER, "c": NON_ADOPTER}, "2\n").run()
        self.assertNonZero(r)

    def test_non_adopter_is_not_counted(self):
        # control: 1 adopter + 1 non-adopter must not satisfy a baseline of 2
        r = self.fx({"a": ADOPTER, "b": NON_ADOPTER}, "2\n").run()
        self.assertNonZero(r)

    def test_comment_mention_is_not_adoption(self):
        r = self.fx({"a": COMMENT_ONLY}, "1\n").run()
        self.assertNonZero(r)

    def test_zero_baseline_zero_adopters_passes(self):
        self.assertZero(self.fx({"a": NON_ADOPTER}, "0\n").run())


class CannotCountFailsClosed(Base):
    def test_missing_baseline(self):
        self.assertNonZero(self.fx({"a": ADOPTER}, None).run())

    def test_empty_baseline(self):
        self.assertNonZero(self.fx({"a": ADOPTER}, "").run())

    def test_non_integer_baseline(self):
        self.assertNonZero(self.fx({"a": ADOPTER}, "two\n").run())

    def test_no_crates_dir_is_undetermined_not_zero_adopters(self):
        f = self.fx({}, "0\n")
        shutil.rmtree(f.tmp / "crates", ignore_errors=True)
        self.assertNonZero(f.run())

    def test_unparseable_crate_manifest(self):
        r = self.fx({"a": ADOPTER, "b": "[package\nname = \n"}, "1\n").run()
        self.assertNonZero(r)


class ShippedFeature(Base):
    def test_fault_injection_in_normal_dependencies_is_rejected_at_baseline(self):
        r = self.fx({"a": SHIPPED}, "1\n").run()
        self.assertNonZero(r)

    def test_control_dev_dependency_only_is_accepted(self):
        self.assertZero(self.fx({"a": ADOPTER}, "1\n").run())


if __name__ == "__main__":
    unittest.main()
