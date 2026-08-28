#!/usr/bin/env python3
"""Ratchet-detect raw stdlib I/O calls in GATE_CRATES that bypass the shared
fallible boundary (`harness_core::boundary`).

Compass DoD (charter north_star "close the fail-open ENTRANCE with types and
lints", commits 690cc4af/c23ac506) item 4: `crates/harness-core/src/boundary.rs`
gives GATE_CRATES a typed way to cross the 3 fallible boundaries this repo
keeps re-fixing by hand — directory walk, file read, subprocess exec — as
`Determination<T>` instead of the raw stdlib call, so "could not observe"
cannot be silently flattened into "found nothing" (see `boundary.rs`'s own
module doc for the argument). A type existing does nothing if nothing forces
adoption; this script is the mechanical half.

It does NOT require migrating every existing call site today (that's future,
incremental work the ratchet makes trackable, not this script's job). It
enforces the SHAPE of migration: the count of raw calls may not RISE above a
committed baseline. A regression (new raw call added) fails; so does an
unlocked improvement (a call site migrated away, dropping the count) — the
latter must be locked in by re-pinning the baseline, mirroring
`check-fail-open.py --ratchet`'s exact contract (this script is deliberately
modelled on it: same baseline-file shape, same three exit codes, same
"cannot-determine fails closed" discipline for empty discovery / unreadable
baseline).

Detected patterns (receiver-aware, GATE_CRATES `src/` only, `#[cfg(test)]`
regions and comments excluded):
  * `std::fs::read_dir(` / bare `read_dir(`           — directory walk
  * `std::fs::read_to_string(` / bare `read_to_string(` — file read
  * `.output()` / `.spawn()` / `.status()`            — subprocess exec

`read_to_string`/`read_dir` are matched only when NOT preceded by `.` — `std::fs::read_to_string(path)`
is the free function this script polices; `some_reader.read_to_string(&mut buf)`
is the unrelated `io::Read` trait method (found in propguard/gate.rs,
propguard/git.rs, specguard/main.rs, specguard/forge/main.rs — all subprocess
stdout/stderr capture, not file reads) and is NOT a raw-stdlib-boundary bypass.
Excluding it is load-bearing: an earlier, receiver-blind version of this
pattern over-counted by 4 (81 vs the correct 77 — see the anti-vacuity
docstring in `scripts/test_check_raw_io_ratchet.py` for the reproduction).

They are also excluded when immediately preceded by `boundary::` —
`harness_core::boundary::read_to_string(path)` / `boundary::read_to_string(path)`
is the module-qualified free function that call sites migrate TO, not a raw
bypass. Without this exclusion the script cannot see its own DoD item 6
progress: the first migration commits (mutategate/stuckguard/propguard raw-IO
burn-down, 2026-07-23) still matched verbatim, holding the count at 77 despite
correct migrations landing (see backlog entries filed by that run's workers,
who independently rediscovered this gap).

Subprocess exec is matched on the TERMINAL method call (`.output()` /
`.spawn()` / `.status()`), not on `Command::new(` construction. An earlier
version of this gate matched `Command::new(` directly, on the theory that a
subprocess call site is "constructed once, unconditionally raw." That theory
is wrong: `harness_core::boundary::run(cmd: &mut Command) -> Determination<...>`
takes an ALREADY-BUILT `Command` — it does not construct one — so every call
site, migrated or not, must still write `Command::new(...)` to produce the
value `boundary::run` consumes. Matching `Command::new(` therefore can never
distinguish "raw" from "migrated"; it is structurally the wrong signal and
holds the count artificially high forever (confirmed 2026-07-23:
`crates/stuckguard/src/anchor.rs`'s 3 sites were migrated to `boundary::run`
in commit ac4af8ef — cargo test/clippy green — but stayed in `--list` under
the old pattern because `Command::new("overwatch")`/`Command::new("condukt")`
still appear as construction lines). The actual raw-exec signal is the
terminal method a raw call site invokes DIRECTLY on the `Command`/builder
(`.output()`, `.spawn()`, or `.status()`, always empty-argument by signature);
code that hands the same `Command` to `boundary::run` instead never calls
these methods in GATE_CRATES' own source (the call happens inside
`harness_core::boundary`, a different crate not in `GATE_CRATES`). Checked
before adopting this pattern: `grep -rn '\\.output()\\|\\.spawn()\\|\\.status()'`
across every GATE_CRATES `src/` turns up only `std::process::Command`/`Child`
receivers — no `reqwest`/`hyper`/HTTP-response `.status()`, no other type in
this codebase exposes these three method names — so the receiver-agnostic
match (no type-narrowing needed, unlike the `read_to_string`/`io::Read`
collision above) is safe here. Being method calls, they need no separate
receiver-blind exclusion the way `read_to_string`/`read_dir` do (a bare,
qualifier-free `.output()`/`.spawn()`/`.status()` call is by construction only
reachable via `.`).

Usage:
  python3 scripts/check-raw-io-ratchet.py                # gate surface, blocking (exit 1 on drift, 2 undetermined)
  python3 scripts/check-raw-io-ratchet.py --ratchet       # same as default (kept for check-fail-open.py symmetry)
  python3 scripts/check-raw-io-ratchet.py --update-baseline  # re-pin baseline to current count
  python3 scripts/check-raw-io-ratchet.py --list          # print every counted call site (file:line), exit 0 always
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

BASELINE_FILE = REPO / "scripts" / "check-raw-io-ratchet.baseline"

# Same canonical set `check-fail-open.py`/`check-gate-crates-sync.py` track.
# Tuple syntax (not list) for the same reason check-fail-open.py's copy is a
# tuple: `check-gate-crates-sync.py`'s `python_const_crates()` extractor only
# recognizes `GATE_CRATES = (...)`.
#
# Registered in that checker's SOURCES as an "exact" source (backlog fb6b1796):
# this copy had silently lost `taintguard`, so every raw stdlib I/O call in the
# newest GATE crate's `src/` was outside the ratchet's scope entirely — the floor
# it printed was a floor over 6 crates while the fleet had 7.
GATE_CRATES = (
    "blastguard",
    "propguard",
    "specguard",
    "stuckguard",
    "mutategate",
    "overwatch",
    "parallelguard",
)

# Receiver-aware: `(?<!\.)` excludes `some_reader.read_to_string(&mut buf)` (the
# io::Read trait method) and any hypothetical `.read_dir()` method call, leaving
# only the free-function forms (`std::fs::read_dir(`, bare `read_dir(` via a
# `use` import). `(?<!boundary::)` additionally excludes the module-qualified
# form calls migrate TO (`harness_core::boundary::read_to_string(` /
# `boundary::read_to_string(` after a `use harness_core::boundary;`) — both
# lookbehinds are fixed-width (required by `re`) and independent, so either one
# firing suppresses the match.
#
# Subprocess exec is matched on the terminal, empty-argument method call
# (`.output()` / `.spawn()` / `.status()`) instead of `Command::new(`
# construction — see the module docstring for why `Command::new(` cannot
# distinguish raw from `boundary::run`-migrated call sites. These are always
# method calls (the leading `\.` is part of the pattern, not a lookbehind), so
# no receiver-blind exclusion is needed the way `read_to_string`/`read_dir`
# need one.
RAW_IO_PATTERN = re.compile(
    r"(?<!\.)(?<!boundary::)\bread_dir\s*\(|(?<!\.)(?<!boundary::)\bread_to_string\s*\("
    r"|\.output\s*\(\s*\)|\.spawn\s*\(\s*\)|\.status\s*\(\s*\)"
)


class BaselineError(Exception):
    """Raised when the baseline cannot be trusted (missing/malformed file, or
    empty file discovery). The caller must fail CLOSED, never default to 0."""


def _tracked_files() -> set[Path] | None:
    """Git-tracked files (absolute), or None if git is unavailable."""
    try:
        out = subprocess.run(
            ["git", "-C", str(REPO), "ls-files", "-z"],
            capture_output=True, text=True, check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return None
    return {REPO / f for f in out.split("\0") if f}


def iter_target_files() -> list[Path]:
    """Every GATE_CRATES `src/**/*.rs` file, git-tracked, sorted for stable
    output."""
    tracked = _tracked_files()
    seen: set[Path] = set()
    out: list[Path] = []
    for crate in GATE_CRATES:
        for p in (REPO / "crates" / crate / "src").rglob("*.rs"):
            if not p.is_file() or p in seen:
                continue
            if tracked is not None and p not in tracked:
                continue
            seen.add(p)
            out.append(p)
    return sorted(out)


def _strip_for_braces(line: str) -> str:
    """Drop `//` comments and string/char literals so brace counting for
    `#[cfg(test)]` region detection is not fooled by braces inside them."""
    line = re.sub(r"//.*$", "", line)
    line = re.sub(r'"(?:[^"\\]|\\.)*"', '""', line)
    line = re.sub(r"'(?:[^'\\]|\\.)'", "''", line)
    return line


def test_region_lines(lines: list[str]) -> set[int]:
    """0-based indices of lines inside a `#[cfg(test)]` module, by brace
    matching from the item that follows the attribute to its close. Test code
    legitimately calls these stdlib functions directly to set up fixtures; it
    is not a boundary bypass in production behavior."""
    marked: set[int] = set()
    n = len(lines)
    i = 0
    cfg = re.compile(r"#\[\s*cfg\s*\(\s*(all\s*\(\s*)?test\b")
    while i < n:
        if cfg.search(lines[i]):
            j = i
            depth = 0
            opened = False
            while j < n:
                marked.add(j)
                for ch in _strip_for_braces(lines[j]):
                    if ch == "{":
                        depth += 1
                        opened = True
                    elif ch == "}":
                        depth -= 1
                if opened and depth <= 0:
                    break
                j += 1
            i = j + 1
        else:
            i += 1
    return marked


def _code_of(line: str) -> str:
    """The code portion of a line: '' if it is a pure comment, else the text
    with a trailing `// …` comment removed."""
    stripped = line.lstrip()
    if stripped.startswith("//") or stripped.startswith("/*") or stripped.startswith("*"):
        return ""
    return re.sub(r"//.*$", "", line)


def scan_file_lines(lines: list[str]) -> list[tuple[int, str]]:
    """Core scan logic over an already-split list of lines: return
    [(1-based lineno, source line)] for raw-IO call sites, excluding
    `#[cfg(test)]` regions and comments. Split out from `scan_file` so tests
    can exercise the scanning logic directly against small fixtures without
    touching the filesystem."""
    test_lines = test_region_lines(lines)
    hits: list[tuple[int, str]] = []
    for idx, line in enumerate(lines):
        if idx in test_lines:
            continue
        code = _code_of(line)
        if RAW_IO_PATTERN.search(code):
            hits.append((idx + 1, line.rstrip("\n")))
    return hits


def scan_file(path: Path) -> list[tuple[int, str]]:
    """Return [(1-based lineno, source line)] for raw-IO call sites in `path`,
    excluding test regions and comments. A file that cannot be read as UTF-8 is
    itself surfaced by the caller counting it as unreadable (see
    `all_gate_crates_count`), never silently skipped."""
    try:
        lines = path.read_text(encoding="utf-8").splitlines(keepends=True)
    except (OSError, UnicodeDecodeError):
        return [(0, f"<unreadable file: {path}>")]
    return scan_file_lines(lines)


def all_gate_crates_count() -> int:
    """Total raw-IO call sites across GATE_CRATES `src/`.

    Fails CLOSED on empty discovery: a zero-file result means the checkout or
    cwd is broken, not that GATE_CRATES has zero call sites (it never has,
    historically) — reading that as a clean 0 would be exactly the
    cannot-determine-as-clean fail-open this whole DoD item exists to close."""
    files = iter_target_files()
    if not files:
        raise BaselineError(
            "GATE_CRATES discovery found ZERO files — cannot compute a "
            "trustworthy raw-IO count (broken checkout / wrong cwd), refusing "
            "to fail open"
        )
    return sum(len(scan_file(p)) for p in files)


def read_baseline(path: Path | None = None) -> int:
    """Read the pinned raw-IO count from the baseline file. Format: exactly one
    non-negative integer line; blank lines and `#`-comments ignored. Missing,
    malformed, or multi-valued files raise BaselineError so the caller fails
    closed — a missing baseline must never default to 0 (that would read
    "cannot determine the floor" as "the floor is zero", making everything
    look like either a spurious improvement or true count itself unreachable)."""
    if path is None:
        path = BASELINE_FILE
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError as e:
        raise BaselineError(f"baseline file not found: {path}") from e
    except OSError as e:
        raise BaselineError(f"baseline file unreadable: {path}: {e}") from e
    nums = [s for s in (ln.strip() for ln in raw.splitlines()) if s and not s.startswith("#")]
    if len(nums) != 1:
        raise BaselineError(
            f"baseline file {path} must contain exactly one integer line, found {len(nums)}"
        )
    try:
        val = int(nums[0])
    except ValueError as e:
        raise BaselineError(f"baseline value is not an integer: {nums[0]!r}") from e
    if val < 0:
        raise BaselineError(f"baseline value is negative: {val}")
    return val


def ratchet_verdict(count: int, baseline: int) -> tuple[int, str]:
    """Pure ratchet comparison. Returns (exit_code, message):
    0 when count == baseline; 1 on ANY drift (a regression above, or an
    unlocked improvement below that must be re-pinned)."""
    if count > baseline:
        return 1, (
            f"raw-io-ratchet: count ROSE {baseline} -> {count}. A new raw "
            f"stdlib read_dir/read_to_string call, or a raw .output()/.spawn()/"
            f".status() subprocess exec, landed in a "
            f"GATE_CRATES src/ tree, bypassing harness_core::boundary. Route it "
            f"through boundary.rs's Determination-returning wrappers instead, "
            f"or, if a reviewed exception, re-pin with --update-baseline (the "
            f"pin is meant to move only DOWN over time; raising it here passes "
            f"the script but is caught in review)."
        )
    if count < baseline:
        return 1, (
            f"raw-io-ratchet: count FELL {baseline} -> {count} — a migration "
            f"to boundary.rs that is not yet locked in. Run `python3 "
            f"scripts/check-raw-io-ratchet.py --update-baseline` and commit "
            f"scripts/check-raw-io-ratchet.baseline so the gain cannot "
            f"silently regress."
        )
    return 0, f"raw-io-ratchet: count == baseline ({baseline}); no new raw-IO call, floor held."


def _write_baseline(count: int) -> None:
    BASELINE_FILE.write_text(f"{count}\n", encoding="utf-8")


def main(argv: list[str]) -> int:
    args = argv[1:]

    if "--list" in args:
        for path in iter_target_files():
            for lineno, text in scan_file(path):
                rel = path.relative_to(REPO)
                print(f"{rel}:{lineno}: {text.strip()}")
        return 0

    if "--update-baseline" in args:
        try:
            count = all_gate_crates_count()
        except BaselineError as e:
            print(f"raw-io-ratchet: cannot compute count to pin: {e}", file=sys.stderr)
            return 2
        _write_baseline(count)
        print(f"raw-io-ratchet: baseline pinned to {count} ({BASELINE_FILE}).")
        return 0

    try:
        count = all_gate_crates_count()
        baseline = read_baseline()
    except BaselineError as e:
        print(
            f"raw-io-ratchet: cannot determine ({e}) — failing closed (exit 2). "
            f"A baseline or count we cannot trust is NOT a pass.",
            file=sys.stderr,
        )
        return 2

    code, msg = ratchet_verdict(count, baseline)
    if code == 1 and count > baseline:
        for path in iter_target_files():
            for lineno, text in scan_file(path):
                rel = path.relative_to(REPO)
                print(f"{rel}:{lineno}: {text.strip()}")
    print(msg, file=sys.stdout if code == 0 else sys.stderr)
    return code


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
