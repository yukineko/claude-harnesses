#!/usr/bin/env python3
"""RED: .githooks/pre-push's `cargo check` lets a test-target compile error through.

Observed by scripts/gate-severity-probe.py on 2026-11-11 (cf954b43): with the
`fault-injection-adoption` fixture (crates/tdd drops `features =
["fault-injection"]` from its harness-core dev-dependency) the pre-push cargo
check exited 0, while `cargo test -p tdd` failed with E0432 (unresolved import
`harness_core::degrade::assert_fails_closed`, gated behind that feature).

Root cause (measured while writing this test, not assumed): pre-push DOES pass
`--all-targets`. But every plugin commit bumps the crate version in lockstep,
which touches Cargo.lock, and a Cargo.lock change in the range widens the check
to `--workspace`. Under `--workspace`, Cargo's feature resolver unifies
harness-core's features across all selected members, so other members'
`fault-injection` dev-dependency switches it on for tdd's test targets too, and
the missing feature in tdd's own manifest is invisible. `cargo check -p tdd`
(and `cargo test -p tdd`) do not unify with other members and fail.

This test reproduces that exact shape in a throwaway `git clone --shared` of
HEAD: the fixture edit plus a tdd micro bump in Cargo.toml and Cargo.lock, then
the cargo check invocation pre-push derives for such a range. The invocation is
read from the hook (the `cargo check ...` line and the `--workspace` widening
on a Cargo.lock change) rather than copied, so the test follows the hook.

Strict xfail => remove the marker when pre-push catches this.

Run:  python3 -m pytest scripts/test_prepush_checks_test_targets_20261111.py
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

import pytest

REPO = Path(__file__).resolve().parent.parent
HOOK = REPO / ".githooks" / "pre-push"
RED = pytest.mark.xfail(
    strict=True,
    reason="RED: pre-push cargo check (--workspace on a Cargo.lock change) unifies "
    "harness-core features, so a test-target-only compile error in tdd passes",
)

FEATURE_DEP = 'harness-core = { path = "../harness-core", features = ["fault-injection"] }'
PLAIN_DEP = 'harness-core = { path = "../harness-core" }'


def hook_cargo_check_argv(hook_text: str) -> list[str]:
    """The `cargo check ...` command line pre-push runs, with its fixed flags."""
    m = re.search(r"if ! (cargo check [^\n;]*?) \$cc_args >&2; then", hook_text)
    if not m:
        raise AssertionError("could not find the cargo check invocation in .githooks/pre-push")
    return m.group(1).split()


def hook_widens_on_cargo_lock(hook_text: str) -> bool:
    """True when a Cargo.lock path in the range sets cc_wide=1 and cc_args=--workspace."""
    lock_arm = re.search(r"^\s*Cargo\.toml\|Cargo\.lock\|[^\n]*\)\s*\n\s*cc_wide=1", hook_text, re.M)
    wide_args = re.search(r'if \[ "\$cc_wide" -eq 1 \]; then\s*\n\s*cc_args="--workspace"', hook_text)
    return bool(lock_arm and wide_args)


def micro_bump(v: str) -> str:
    major, minor, patch = v.split(".")
    return f"{major}.{minor}.{int(patch) + 1}"


class PrePushChecksTestTargets(unittest.TestCase):
    def setUp(self):
        self.tmp = Path(tempfile.mkdtemp(prefix="prepush-test-targets-", dir=tempfile.gettempdir()))
        self.clone = self.tmp / "clone"
        subprocess.run(
            ["git", "clone", "-q", "--shared", str(REPO), str(self.clone)],
            check=True, capture_output=True, text=True,
        )
        subprocess.run(
            ["git", "-C", str(self.clone), "checkout", "-q", "--detach",
             subprocess.run(["git", "-C", str(REPO), "rev-parse", "HEAD"],
                            check=True, capture_output=True, text=True).stdout.strip()],
            check=True, capture_output=True, text=True,
        )

    def tearDown(self):
        shutil.rmtree(self.tmp, ignore_errors=True)

    def _edit(self, rel: str, old: str, new: str) -> None:
        p = self.clone / rel
        text = p.read_text(encoding="utf-8")
        self.assertIn(old, text, f"{rel}: anchor not found")
        p.write_text(text.replace(old, new, 1), encoding="utf-8")

    def _inject(self) -> None:
        # Test-target-only compile error: tdd's tests/fault_injection.rs imports a
        # harness-core item that exists only with the `fault-injection` feature.
        self._edit("crates/tdd/Cargo.toml", FEATURE_DEP, PLAIN_DEP)
        # Lockstep bump as every plugin commit does; this is what puts Cargo.lock
        # in the pushed range.
        cargo = (self.clone / "crates/tdd/Cargo.toml").read_text(encoding="utf-8")
        old = re.search(r'^version = "([^"]+)"', cargo, re.M).group(1)
        new = micro_bump(old)
        self._edit("crates/tdd/Cargo.toml", f'version = "{old}"', f'version = "{new}"')
        self._edit("Cargo.lock", f'name = "tdd"\nversion = "{old}"', f'name = "tdd"\nversion = "{new}"')

    def _cargo(self, argv: list[str]) -> subprocess.CompletedProcess:
        env = dict(os.environ)
        cargo_bin = Path.home() / ".cargo" / "bin"
        env["PATH"] = f"{cargo_bin}{os.pathsep}{env.get('PATH', '')}"
        # Reuse the running checkout's target dir so dependency builds are cached.
        env.setdefault("CARGO_TARGET_DIR", str(REPO / "target"))
        return subprocess.run(argv, cwd=self.clone, env=env, capture_output=True, text=True, timeout=1800)

    @RED
    def test_prepush_cargo_check_fails_on_test_target_compile_error(self):
        hook_text = HOOK.read_text(encoding="utf-8")
        check_argv = hook_cargo_check_argv(hook_text)
        self.assertIn("--all-targets", check_argv)
        self.assertTrue(hook_widens_on_cargo_lock(hook_text),
                        "pre-push no longer widens to --workspace on Cargo.lock; re-anchor this test")
        self._inject()

        # Control: the per-package check sees the error (proves the injection bites).
        ctl = self._cargo(["cargo", "check", "--all-targets", "--quiet", "-p", "tdd"])
        self.assertNotEqual(ctl.returncode, 0, "injection did not break tdd's test target:\n" + ctl.stderr[-2000:])

        # What pre-push runs for a range that touches Cargo.lock.
        res = self._cargo([*check_argv, "--workspace"])
        self.assertNotEqual(
            res.returncode, 0,
            f"pre-push's `{' '.join(check_argv)} --workspace` exited 0 although "
            f"`cargo check --all-targets -p tdd` exited {ctl.returncode}",
        )


if __name__ == "__main__":
    unittest.main()
