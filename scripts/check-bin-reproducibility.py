#!/usr/bin/env python3
"""Verify committed plugin binaries are reproducible from source (tamper gate).

Each plugin ships committed per-platform binaries (`crates/<plugin>/bin/<name>-<os>-<arch>`)
so `/plugin install` needs neither cargo nor an API key. Those binaries are what
actually run on a user's machine — but they are opaque blobs in git review, so a
tampered binary (an exfiltration call, a concealment string baked in) would sail
through code review untouched. This gate rebuilds every workspace binary from the
committed source and compares: any string present in the *committed* binary but
absent from the *freshly built* one, that also matches a malicious pattern, means
the committed blob carries something the source does not produce.

Judgement rule (critical — avoids false alarms): the raw count of committed-only
strings is routinely large (tens to hundreds) purely from build non-determinism —
symbol-metadata hashes and `strings` boundary differences — and from source drift
(a committed binary built from slightly older source). That is NOT tampering. So
this gate judges ONLY the malicious-pattern-filtered delta (shared with
injectguard's `check-prompt-injection.py`), never the raw count or size.

Only host-triple binaries are checked (a Linux CI runner cannot rebuild the macOS
binaries). Self-contained apart from cargo + binutils `strings`.

Usage:
    python3 scripts/check-bin-reproducibility.py            # builds, then checks
    python3 scripts/check-bin-reproducibility.py --no-build # checks existing target/release
Exit 0 = every host binary reproducible (no malicious committed-only delta);
exit 1 = one or more suspicious committed-only strings (all printed).
"""

from __future__ import annotations

import importlib.util
import json
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# Reuse injectguard's malicious patterns so there is ONE source of truth for what
# "suspicious" means across the prompt-asset gate and the binary gate.
_SPEC = importlib.util.spec_from_file_location(
    "check_prompt_injection", REPO / "scripts" / "check-prompt-injection.py"
)
ig = importlib.util.module_from_spec(_SPEC)
_SPEC.loader.exec_module(ig)


def host_os_arch() -> tuple[str, str, str]:
    """Return (os, arch, rustc_triple) normalised the same way build-plugin-bin.sh
    names committed binaries."""
    out = subprocess.run(
        ["rustc", "-vV"], capture_output=True, text=True, check=True
    ).stdout
    host = ""
    for ln in out.splitlines():
        if ln.startswith("host: "):
            host = ln[len("host: "):].strip()
    os_ = ("darwin" if "apple-darwin" in host else
           "linux" if "linux" in host else
           "windows" if "windows" in host else "unknown")
    arch = ("x86_64" if host.startswith("x86_64-") else
            "arm64" if host.startswith("aarch64-") else "unknown")
    return os_, arch, host


def target_release_dir() -> Path:
    try:
        md = subprocess.run(
            ["cargo", "metadata", "--no-deps", "--format-version=1"],
            capture_output=True, text=True, check=True,
        ).stdout
        td = json.loads(md).get("target_directory")
        if td:
            return Path(td) / "release"
    except (OSError, subprocess.CalledProcessError, json.JSONDecodeError):
        pass
    return REPO / "target" / "release"


def strings_of(path: Path, minlen: int = 5) -> set[str]:
    try:
        out = subprocess.run(
            ["strings", "-n", str(minlen), str(path)],
            capture_output=True, text=True, check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return set()
    return set(out.splitlines())


def committed_host_bins(os_: str, arch: str) -> list[tuple[Path, str]]:
    """[(committed binary path, cargo bin/artifact name)] for the host platform."""
    suffix = f"-{os_}-{arch}"
    out: list[tuple[Path, str]] = []
    for p in sorted(REPO.glob("crates/*/bin/*" + suffix)):
        if p.is_file():
            out.append((p, p.name[: -len(suffix)]))
    return out


def suspicious_committed_only(
    committed_strings: set[str], fresh_strings: set[str]
) -> list[tuple[str, str]]:
    """Committed-only strings that match a malicious pattern. Pure — this is the
    unit-testable core, independent of cargo/strings."""
    hits: list[tuple[str, str]] = []
    for s in sorted(committed_strings - fresh_strings):
        name = ig.malicious_without_defense(s)
        if name:
            hits.append((s, name))
    return hits


def main(argv: list[str]) -> int:
    do_build = "--no-build" not in argv[1:]
    os_, arch, triple = host_os_arch()
    rel = target_release_dir()

    if do_build:
        print(f"building workspace bins (host {triple}) ...", file=sys.stderr)
        subprocess.run(
            ["cargo", "build", "--release", "--workspace", "--bins"],
            check=True,
        )

    bins = committed_host_bins(os_, arch)
    if not bins:
        print(f"bin-reproducibility: no committed host binaries for {os_}-{arch}; "
              f"nothing to check.")
        return 0

    total = 0
    checked = 0
    skipped: list[str] = []
    for committed, binname in bins:
        fresh = rel / binname
        if not fresh.exists():
            skipped.append(binname)
            continue
        checked += 1
        hits = suspicious_committed_only(strings_of(committed), strings_of(fresh))
        for s, name in hits:
            relp = committed.relative_to(REPO)
            print(f"{relp}: committed-only suspicious string [{name}]: {s!r}")
            total += 1

    if skipped:
        print(f"bin-reproducibility: no fresh build for {len(skipped)} bin(s) "
              f"(build them or drop --no-build): {', '.join(sorted(skipped))}",
              file=sys.stderr)

    if total:
        print(f"\nbin-reproducibility: {total} suspicious committed-only string(s) "
              f"across {checked} host binaries — the committed blob carries "
              f"content its source does not produce. Investigate for tampering. "
              f"(Raw committed-only counts and size deltas are ignored; only "
              f"malicious-pattern matches are reported.)", file=sys.stderr)
        return 1

    print(f"bin-reproducibility: {checked} host binaries reproducible "
          f"(no malicious committed-only delta).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
