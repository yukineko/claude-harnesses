#!/usr/bin/env python3
"""Run the trybuild compile-fail type-contract tests and fail closed.

The `harness_core::verdict` / scope type contracts ("a fail-open is
unrepresentable") are pinned by trybuild tests that assert certain code does NOT
compile, with the exact rustc diagnostics committed as `.stderr` snapshots. A
plain `cargo check` (the pre-push type-check) never builds those fixtures, so
without this script nothing at an enforcement point ever observes the contract.
Backlog 034b6620.

Exactly these test targets are run (one `cargo test -p <crate> --test <target>`
each); every one must report `test result: ok. N passed` with N equal to the
expected number of #[test] fns:

    harness-core  verdict_compile_fail   (compile_fail tests/ui/verdict + pass verdict_pass)
    harness-core  scope_compile_fail     (pass tests/ui/scope_pass — positive control)
    blastguard    verdict_compile_fail
    donegate      verdict_compile_fail

Exit codes:
    0  every target ran and passed
    1  a target failed (a fixture compiled, or a diagnostic drifted from its .stderr)
    2  UNDETERMINED — cargo/rustc missing, the active rustc is not the pinned
       rust-toolchain.toml channel, cargo exited abnormally without a test result,
       or the expected test count was not observed. Undetermined is not clean:
       it is non-zero on purpose (CLAUDE.md section 3).

`TRYBUILD` is removed from the child environment: `TRYBUILD=overwrite` makes
trybuild rewrite the snapshots and report success, which would turn this check
into a no-op.
"""

from __future__ import annotations

import os
import re
import shutil
import subprocess
import sys
from pathlib import Path

try:
    import tomllib  # Python >= 3.11
except ModuleNotFoundError:  # older python3 (e.g. /usr/bin/python3 on macOS)
    tomllib = None  # type: ignore[assignment]

# (package, test target, expected number of passing #[test] fns)
TARGETS: list[tuple[str, str, int]] = [
    ("harness-core", "verdict_compile_fail", 1),
    ("harness-core", "scope_compile_fail", 1),
    ("blastguard", "verdict_compile_fail", 1),
    ("donegate", "verdict_compile_fail", 1),
]

RESULT_RE = re.compile(r"^test result: (ok|FAILED)\. (\d+) passed; (\d+) failed", re.M)


def undetermined(msg: str) -> int:
    print(f"check-compile-fail: UNDETERMINED — {msg}", file=sys.stderr)
    print("check-compile-fail: blocking; 'could not run' is not 'passed'.", file=sys.stderr)
    return 2


def pinned_channel(root: Path) -> str | None:
    if tomllib is None:
        return None
    try:
        with open(root / "rust-toolchain.toml", "rb") as f:
            return str(tomllib.load(f)["toolchain"]["channel"])
    except (OSError, tomllib.TOMLDecodeError, KeyError, TypeError):
        return None


def main() -> int:
    root = Path(__file__).resolve().parent.parent
    for exe in ("cargo", "rustc"):
        if shutil.which(exe) is None:
            return undetermined(f"{exe} not found on PATH (source \"$HOME/.cargo/env\")")

    channel = pinned_channel(root)
    if channel is None:
        return undetermined(
            "rust-toolchain.toml missing, unparseable, or has no [toolchain].channel "
            "(or this python3 lacks tomllib; need >= 3.11)"
        )
    try:
        ver = subprocess.run(
            ["rustc", "--version"], cwd=root, capture_output=True, text=True, check=False
        )
    except OSError as e:
        return undetermined(f"could not run rustc: {e}")
    if ver.returncode != 0 or not ver.stdout.startswith(f"rustc {channel} "):
        return undetermined(
            f"active rustc is {ver.stdout.strip() or ver.stderr.strip()!r}, but the "
            f".stderr snapshots are pinned to {channel} (rust-toolchain.toml)"
        )

    env = {k: v for k, v in os.environ.items() if k != "TRYBUILD"}
    failed: list[str] = []
    for pkg, test, expected in TARGETS:
        label = f"{pkg}::{test}"
        print(f"check-compile-fail: running {label} ...", file=sys.stderr)
        try:
            proc = subprocess.run(
                ["cargo", "test", "-p", pkg, "--test", test],
                cwd=root,
                env=env,
                capture_output=True,
                text=True,
                check=False,
            )
        except OSError as e:
            return undetermined(f"could not run cargo for {label}: {e}")
        out = proc.stdout + proc.stderr
        results = RESULT_RE.findall(out)
        if len(results) != 1:
            sys.stderr.write(out)
            return undetermined(
                f"{label}: cargo exited {proc.returncode} with {len(results)} "
                "test-result lines (build error or target missing)"
            )
        status, passed, nfailed = results[0][0], int(results[0][1]), int(results[0][2])
        if proc.returncode == 0 and status == "ok" and passed == expected and nfailed == 0:
            print(f"check-compile-fail: ok {label} ({passed} passed)", file=sys.stderr)
            continue
        if status == "ok" and proc.returncode == 0:
            sys.stderr.write(out)
            return undetermined(f"{label}: expected {expected} passing test(s), saw {passed}")
        sys.stderr.write(out)
        print(f"check-compile-fail: FAILED {label} (cargo exit {proc.returncode})", file=sys.stderr)
        failed.append(label)

    if failed:
        print(
            "check-compile-fail: RED — type-contract compile-fail test(s) failed: "
            + ", ".join(failed)
            + ". A fixture that now compiles means the contract reopened; a drifted "
            "diagnostic means the .stderr must be regenerated deliberately with the "
            "pinned toolchain (TRYBUILD=overwrite) and reviewed.",
            file=sys.stderr,
        )
        return 1
    print(f"check-compile-fail: all {len(TARGETS)} trybuild targets passed", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
