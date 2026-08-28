#!/usr/bin/env python3
"""Scan gate code for the *fail-open swallow* patterns this repo keeps fixing.

The house doctrine is that a gate which resolves "cannot determine" into
"fine / clean / allow / empty" is worse than no gate ("fail-open するくらい
ないほうがまし"). Across many hardening rounds the SAME few code shapes kept
producing that fail-open, each found by hand:

  * a directory walk that swallows an unreadable subtree — `let Ok(entries) =
    std::fs::read_dir(dir) else { return/continue };` and `entries.flatten()` —
    so an unreadable subtree is silently dropped from the walk, the collection
    comes back short, and "0 findings / fewer files" reads as clean. This is the
    exact shape fixed in `specguard` testaudit (round #7, 05df9b2), `specforge`
    gather (round #11, 02b80c6), and `decision.rs::list_files` before them; the
    FIXED form is `match read_dir(dir) { Ok(e)=>e, Err(e) if e.kind()==NotFound
    => .. , Err(e) => <surface/propagate> }` + a per-entry `match entry { Ok..,
    Err.. }` (never `.flatten()`).
  * a shell gate that captures a command through `$(… 2>/dev/null)` and lets a
    non-zero exit vanish, so a git/tool failure yields an empty string that is
    then read as "nothing changed → nothing to test → pass" (round #6,
    231e20e, `test-changed-crates.sh`). The FIXED form captures the rc
    (`|| rc=$?`) and fails closed when it is non-zero.

This script is the missing *mechanical* gate: instead of finding the next one
by hand, it fails CI when a new instance of these shapes lands. It is modelled
on `check-prompt-injection.py` (injectguard): stdlib-only, git-tracked scoping,
`file:line` output, exit 1 on any un-allowlisted hit, and — like injectguard's
non-UTF-8 handling — a file it cannot read is itself a finding, never silently
clean.

False-positive discipline (load-bearing, or the gate gets disabled): the
patterns are matched RECEIVER-AWARE, not by a bare keyword. `.flatten()` is only
flagged when a `read_dir(` sits within a few lines above it (the walk idiom);
`.flatten()` on an `Option`/`Vec<Vec<_>>` is ignored. `2>/dev/null` is only
flagged inside a `$(…)`/backtick capture whose rc is NOT recovered (`|| rc=$?`,
`|| true`, `command -v` probes are ignored). `#[cfg(test)]` modules and comment
lines are excluded. Deliberately fail-soft helpers that return `Option`/empty to
*signal* failure to a caller (e.g. `Command…output().ok()?` in a fn named
`*_soft`) are NOT matched — a line-level scan cannot tell whether the caller
treats `None` as fail-open, so only the SILENT-SWALLOW shapes (where the error
is discarded and the fallback is indistinguishable from a real empty result) are
in scope. Genuine known exceptions are carried in ALLOWLIST with a reason.

Scope. By default only the GATE surface is scanned and enforced (merge-blocking):
the gate crates' `src/` and `scripts/*.sh`, where a fail-open is load-bearing.
`--all` widens the scan to every crate's `src/` but is ADVISORY (exit 0) — a
raw discovery aid, not a merge gate, so pre-existing swallows in non-gate crates
do not block unrelated work.

`--ratchet` turns that whole-workspace count into an enforced burn-down WITHOUT
demanding every pre-existing swallow be fixed at once: it compares the live
`--all` count against a committed baseline (scripts/check-fail-open.baseline) and
fails on any drift — a count ABOVE the baseline is a new swallow (regression),
and a count BELOW it is an improvement that must be locked in by lowering the
baseline in the same change. So a new swallow cannot land without EITHER a fix
OR a visible, reviewed edit to the committed baseline. Note the boundary: this
script enforces "live count == pinned number" in BOTH directions; it does NOT
by itself compare the baseline against its prior value, so a PR that RAISES the
pin (to admit new swallows) still passes. That the pin only ever *decreases*
over time is enforced by human review of its one-line diff, not by this gate.
A missing/malformed baseline or an empty workspace
discovery is a cannot-determine and fails CLOSED (exit 2), never a pass — the
advisory `--all` was itself a fail-open (it detects but exits 0), and this is the
enforcement half. `--update-baseline` re-pins the baseline to the current count.

Usage:
  python3 scripts/check-fail-open.py            # gate surface, blocking (exit 1 on hit)
  python3 scripts/check-fail-open.py --all      # whole workspace, advisory (exit 0)
  python3 scripts/check-fail-open.py --ratchet  # enforce burn-down vs baseline (0 hold / 1 drift / 2 undetermined)
  python3 scripts/check-fail-open.py --update-baseline  # re-pin baseline to current count
  python3 scripts/check-fail-open.py <file>...   # scan explicit files (blocking)
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The committed ratchet baseline: the pinned count of un-allowlisted fail-open
# swallows across the whole workspace (`--all`). `--ratchet` fails when the live
# count drifts from this number in EITHER direction. The pin's direction (that it
# only decreases over time) is enforced by review of its one-line diff, not by
# the script — nothing here compares the baseline against its prior value.
BASELINE_FILE = REPO / "scripts" / "check-fail-open.baseline"

# The crates whose gates guard the fleet (CLAUDE.md GATE_CRATES). A fail-open
# here is load-bearing, so these are the merge-blocking surface.
#
# Tuple syntax (not a list) is deliberate: scripts/check-gate-crates-sync.py's
# `python_const_crates()` extractor only recognizes `GATE_CRATES = (...)`
# (the shape check-plugin-rollout.py and check-fail-open-mutation.py already
# use), so this copy is now tracked as that script's 9th SOURCES entry
# (backlog bb667ce1) — keep it a tuple or the sync checker stops parsing it.
GATE_CRATES = (
    "blastguard",
    "propguard",
    "specguard",
    "stuckguard",
    "taintguard",
    "mutategate",
    "overwatch",
    "parallelguard",
)

# How many code lines above a `.flatten()` we look for the `read_dir(` that makes
# it a directory-walk swallow (the idiom is `let Ok(x)=read_dir(..) else {..};
# for e in x.flatten()` — read_dir and flatten sit within a few lines).
READDIR_WINDOW = 6

# ── Rust patterns ───────────────────────────────────────────────────────────

# `let Ok(..) = <expr with read_dir(> .. else {` — the read_dir let-else swallow.
# The fail-closed form uses `match read_dir(..) { Ok(..)=> , Err(e) if
# ..NotFound => , Err(e) => .. }`, which has no `let Ok(..) = .. else`, so this
# does not match the correct form.
RS_READDIR_LET_ELSE = re.compile(r"\blet\s+Ok\s*\(.*\bread_dir\s*\(.*\belse\b")

# A bare `.flatten()` call — flagged only when a read_dir sits within the window
# above it (see `scan_rust`), so an Option/Vec flatten is not a false positive.
RS_FLATTEN = re.compile(r"\.flatten\s*\(\s*\)")
RS_READDIR = re.compile(r"\bread_dir\s*\(")

# ── the empty-collection fallback class (backlog b0cacd15, added 2026-08-06) ──
#
# The two read_dir shapes above are the ones this scanner shipped with, and the
# pinned baseline of 0 was read for a long time as "zero fail-open". It was not:
# it was "zero of those two shapes". The class below is the one CLAUDE.md §3
# names outright — エラー時に空の集合を返さない — because downstream reads an
# empty result as "nothing to inspect, therefore clean". It is scored on the
# ADVISORY / `--ratchet` surface only (see ADVISORY_ONLY_PATTERNS).

# `Err(_) => Vec::new()` / `Err(_) => Ok(Vec::new())` / `Err(e) => X::default()`
# — the error is discarded and an EMPTY value takes its place. The fixed forms
# (`Err(e) => Err(e)`, `Err(e) => Determination::undetermined(..)`) do not match:
# they substitute nothing, they propagate or name the third state.
RS_ERR_ARM_EMPTY = re.compile(
    r"\bErr\s*\(\s*[_A-Za-z]\w*\s*\)\s*=>\s*"
    r"(?:Ok\s*\(\s*|Some\s*\(\s*)?"
    r"(?:Vec::new\s*\(\s*\)|String::new\s*\(\s*\)|HashMap::new\s*\(\s*\)"
    r"|HashSet::new\s*\(\s*\)|BTreeMap::new\s*\(\s*\)|BTreeSet::new\s*\(\s*\)"
    r"|VecDeque::new\s*\(\s*\)|vec!\s*\[\s*\]"
    r"|Default::default\s*\(\s*\)|[A-Za-z_]\w*::default\s*\(\s*\))"
)

# `.unwrap_or_default()` / `.unwrap_or(false)` / `.unwrap_or(Vec::new())` on the
# result of a filesystem read — flagged only when one of the IO calls below sits
# within the window above (RECEIVER-AWARE, same discipline as `.flatten()`), so
# `s.parse().unwrap_or_default()` is not a false positive. Named verbatim in
# b0cacd15: harness-status `plugins.rs::dir_nonempty` (unreadable dir → the same
# `false` as an empty one) and `path_shadow.rs::list_binary_names`.
RS_UNWRAP_OR_EMPTY = re.compile(
    r"\.unwrap_or_default\s*\(\s*\)"
    r"|\.unwrap_or\s*\(\s*(?:false|0|Vec::new\s*\(\s*\)|String::new\s*\(\s*\)"
    r"|vec!\s*\[\s*\]|Default::default\s*\(\s*\))\s*\)"
    r"|\.unwrap_or_else\s*\(\s*\|_\|\s*(?:Vec::new\s*\(\s*\)|String::new\s*\(\s*\)"
    r"|vec!\s*\[\s*\])"
)
RS_IO_CALL = re.compile(
    r"\b(?:read_dir|read_to_string|read_link|metadata|symlink_metadata"
    r"|canonicalize|File::open|fs::read)\s*\("
)

# Form B — the per-record variant of the same erasure: a loop that parses lines
# and pushes only the `Ok` ones, so a truncated/corrupt ledger comes back as a
# SHORTER history rather than an unreadable one. Requires the loop above AND the
# push below, so a plain `if let Ok(cfg) = toml::from_str(..)` is not matched.
RS_IF_LET_OK = re.compile(r"\bif\s+let\s+Ok\s*\(")
RS_FOR_LOOP = re.compile(r"\bfor\s+\w+\s+in\b")
RS_PUSH = re.compile(r"\.(?:push|insert|extend)\s*\(")
LOOP_WINDOW = 4
PUSH_WINDOW = 3

# Pattern names scored on the ADVISORY / `--ratchet` surface but deliberately
# kept OUT of the merge-blocking gate-surface verdict (decision of 2026-08-06).
#
# Rationale, recorded so a later reader does not "tidy" it away: this class fires
# on ~9 pre-existing overwatch sites at once. Putting it in the blocking path
# would make the only way through a commit an ALLOWLIST entry per site — which
# converts the reviewed-exception hatch into the default escape route, exactly
# what CLAUDE.md §5 forbids ("skip 機構は理由を書いて一度だけ。恒常的な迂回に
# 使わない"). The burn-down pressure is the baseline diff instead: a NEW instance
# raises the live count above the pin and `--ratchet` fails, while removing one
# forces a visible, reviewed edit to the pinned number. Hits are still PRINTED on
# every run, tagged `advisory`; they are silent nowhere.
ADVISORY_ONLY_PATTERNS = frozenset(
    {"err-arm-empty-fallback", "read-unwrap-or-empty", "loop-parse-drop"}
)


def blocking_hits(hits: list[tuple[int, str, str]]) -> list[tuple[int, str, str]]:
    """The subset of `hits` that counts toward a merge-BLOCKING verdict.

    Drops the advisory-only class (see ADVISORY_ONLY_PATTERNS). This is the ONLY
    filter: `all_crates_count`/`--ratchet` deliberately do not call it, so an
    advisory-class regression still moves the pinned number."""
    return [h for h in hits if h[2] not in ADVISORY_ONLY_PATTERNS]

# ── shell patterns ──────────────────────────────────────────────────────────

# A command substitution `$(… 2>/dev/null …)` or `` `… 2>/dev/null …` `` — the
# stderr-and-exit-swallowing capture. Suppressed (see `scan_shell`) when the rc
# is recovered on the same line (`|| rc=$?`, `|| true`) or it is a `command -v`
# probe.
SH_CAPTURE_DEVNULL = re.compile(r"(\$\(|`)[^\n]*2>\s*/dev/null")
SH_RC_RECOVERED = re.compile(r"\|\|\s*(\w+=\$\?|true\b|:\s|\{)")
SH_PROBE = re.compile(r"\bcommand\s+-v\b|\btype\s+-p\b|\bwhich\b")

# ── allowlist ───────────────────────────────────────────────────────────────
# Genuine, reviewed exceptions. Each entry suppresses a hit whose (relpath,
# pattern, and `needle` substring of the offending line) all match. Every entry
# MUST carry a reason; a "filed" reason names the backlog id tracking the real
# fix (grandfathered so this detector can ship and block NEW instances — never a
# silent excuse).
ALLOWLIST: list[dict[str, str]] = [
    # (Round #19 / 50ad2c1e resolved: the overwatch test_freshness.rs `.rs` walk
    # now fails closed via `rust_source_files -> io::Result` + `IgnoredTestLookup
    # ::ScanIncomplete` — the read_dir let-else and `entries.flatten()` swallows
    # were removed, so the two grandfathered entries here are gone with them.)
    # BENIGN (reviewed): the round-#6 unborn-branch in test-changed-crates.sh.
    # This `git ls-files` runs ONLY after `! git rev-parse --verify HEAD` has
    # proven the repo is on an unborn branch — a fully-determinable "no baseline,
    # everything is new" state — and deliberately sets diff_rc=0. It is the
    # legitimate-absent path, not a cannot-determine swallow.
    {
        "path": "scripts/test-changed-crates.sh",
        "pattern": "shell-devnull-capture-swallow",
        "needle": "git ls-files 2>/dev/null",
        "reason": "benign: unborn-branch legitimate-absent (round #6, 231e20e), rc set to 0 intentionally after a verified rev-parse guard",
    },
]

# Sentinel line number for a whole-file finding (unreadable / undecodable), so a
# file we cannot vouch for goes red rather than silently clean.
UNREADABLE = -1


def _tracked_files() -> set[Path] | None:
    """Git-tracked files (absolute), or None if git is unavailable. Keeps nested
    worktrees, the plugin cache, and untracked scratch out of scope."""
    try:
        out = subprocess.run(
            ["git", "-C", str(REPO), "ls-files", "-z"],
            capture_output=True, text=True, check=True,
        ).stdout
    except (OSError, subprocess.CalledProcessError):
        return None
    return {REPO / f for f in out.split("\0") if f}


def iter_target_files(all_crates: bool) -> list[Path]:
    """Rust + shell files to scan. Gate surface by default; every crate's src/
    plus scripts when `all_crates`."""
    tracked = _tracked_files()
    globs: list[str] = ["scripts/*.sh"]
    if all_crates:
        globs.append("crates/*/src/**/*.rs")
    else:
        globs += [f"crates/{c}/src/**/*.rs" for c in GATE_CRATES]
    seen: set[Path] = set()
    out: list[Path] = []
    for glob in globs:
        for p in REPO.glob(glob):
            if not p.is_file() or p in seen:
                continue
            if tracked is not None and p not in tracked:
                continue
            seen.add(p)
            out.append(p)
    return sorted(out)


def _strip_for_braces(line: str) -> str:
    """Crudely drop `//` line comments and string/char literals so brace counting
    for test-module detection is not fooled by braces inside them. Good enough
    for well-formed Rust; the gate errs toward scanning (not excluding) on doubt."""
    # remove line comment
    line = re.sub(r"//.*$", "", line)
    # remove string and char literals (non-greedy, no escaped-quote handling —
    # sufficient for brace balance in practice)
    line = re.sub(r'"(?:[^"\\]|\\.)*"', '""', line)
    line = re.sub(r"'(?:[^'\\]|\\.)'", "''", line)
    return line


def test_region_lines(lines: list[str]) -> set[int]:
    """0-based indices of lines inside a `#[cfg(test)]` module, by brace matching
    from the item that follows the attribute to its close. Excluded from scanning
    (test code legitimately uses these shapes)."""
    marked: set[int] = set()
    n = len(lines)
    i = 0
    cfg = re.compile(r"#\[\s*cfg\s*\(\s*(all\s*\(\s*)?test\b")
    while i < n:
        if cfg.search(lines[i]):
            # advance to the first '{' at/after the attribute, then brace-match.
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
    """The code portion of a line: '' if it is a pure comment, else the text with
    a trailing `// …` comment removed. Keeps a pattern that sits in a comment from
    tripping the gate while still catching code that has a trailing comment."""
    stripped = line.lstrip()
    if stripped.startswith("//") or stripped.startswith("/*") or stripped.startswith("*"):
        return ""
    return re.sub(r"//.*$", "", line)


def scan_rust(lines: list[str]) -> list[tuple[int, str, str]]:
    """Return [(1-based lineno, code, pattern_name)] for read_dir-walk swallows."""
    hits: list[tuple[int, str, str]] = []
    test_lines = test_region_lines(lines)
    code = [("" if i in test_lines else _code_of(l)) for i, l in enumerate(lines)]
    for idx, c in enumerate(code):
        if not c:
            continue
        if RS_READDIR_LET_ELSE.search(c):
            hits.append((idx + 1, lines[idx].rstrip("\n"), "readdir-let-else-swallow"))
        if RS_FLATTEN.search(c):
            lo = max(0, idx - READDIR_WINDOW)
            if any(RS_READDIR.search(code[j]) for j in range(lo, idx + 1)):
                hits.append((idx + 1, lines[idx].rstrip("\n"), "readdir-flatten-swallow"))
        # ── advisory class (b0cacd15): an error erased into an EMPTY value ──
        if RS_ERR_ARM_EMPTY.search(c):
            hits.append((idx + 1, lines[idx].rstrip("\n"), "err-arm-empty-fallback"))
        if RS_UNWRAP_OR_EMPTY.search(c):
            lo = max(0, idx - READDIR_WINDOW)
            if any(RS_IO_CALL.search(code[j]) for j in range(lo, idx + 1)):
                hits.append((idx + 1, lines[idx].rstrip("\n"), "read-unwrap-or-empty"))
        if RS_IF_LET_OK.search(c):
            lo = max(0, idx - LOOP_WINDOW)
            hi = min(len(code), idx + 1 + PUSH_WINDOW)
            in_loop = any(RS_FOR_LOOP.search(code[j]) for j in range(lo, idx))
            collects = any(RS_PUSH.search(code[j]) for j in range(idx + 1, hi))
            if in_loop and collects:
                hits.append((idx + 1, lines[idx].rstrip("\n"), "loop-parse-drop"))
    return hits


def _next_code_line(lines: list[str], idx: int) -> str:
    """The next non-blank, non-comment line after idx (''+ if none)."""
    for j in range(idx + 1, len(lines)):
        s = lines[j].strip()
        if s and not s.startswith("#"):
            return s
    return ""


# The safe idiom captures the exit code on the FOLLOWING line: `VAR=$(… )` then
# `rc=$?` / `AUT_EXIT=$?`. Recognising it keeps the gate from crying wolf on the
# very rc-capturing code that fails closed (e.g. e2e-autonomy.sh's autonomy-check).
SH_RC_NEXTLINE = re.compile(r"^\s*\w+=\$\?")


def scan_shell(lines: list[str]) -> list[tuple[int, str, str]]:
    """Return findings for `$(… 2>/dev/null)` captures whose rc is swallowed.

    A capture is NOT a swallow when its exit code is recovered — on the same line
    (`|| rc=$?`, `|| true`), on the next line (`rc=$?`), or it is a `command -v`
    probe. Only a capture that drops the rc AND lets an empty result read as
    success is a fail-open."""
    hits: list[tuple[int, str, str]] = []
    for idx, raw in enumerate(lines):
        line = raw
        if line.lstrip().startswith("#"):
            continue  # comment
        if not SH_CAPTURE_DEVNULL.search(line):
            continue
        if SH_RC_RECOVERED.search(line) or SH_PROBE.search(line):
            continue  # rc recovered on this line / benign probe
        if SH_RC_NEXTLINE.match(_next_code_line(lines, idx)):
            continue  # rc captured on the following line
        hits.append((idx + 1, raw.rstrip("\n"), "shell-devnull-capture-swallow"))
    return hits


def _allowlisted(relpath: str, name: str, line: str) -> bool:
    for e in ALLOWLIST:
        if e["path"] == relpath and e["pattern"] == name and e["needle"] in line:
            return True
    return False


def scan_file(path: Path) -> list[tuple[int, str, str]]:
    """Scan one file. Unreadable / undecodable → a finding (never silently clean,
    mirroring injectguard). Allowlisted hits are dropped."""
    try:
        raw = path.read_bytes()
    except OSError as e:
        return [(UNREADABLE, f"cannot read gate source: {e}", "unreadable-source")]
    try:
        text = raw.decode("utf-8")
    except UnicodeDecodeError:
        text = raw.decode("utf-8", errors="replace")
    lines = text.splitlines()
    if path.suffix == ".rs":
        raw_hits = scan_rust(lines)
    elif path.suffix == ".sh":
        raw_hits = scan_shell(lines)
    else:
        raw_hits = []
    rel = str(path.relative_to(REPO)) if REPO in path.parents else str(path)
    return [(ln, txt, nm) for (ln, txt, nm) in raw_hits
            if not _allowlisted(rel, nm, txt)]


class BaselineError(Exception):
    """The ratchet baseline or the live count could not be DETERMINED.

    Per house doctrine a cannot-determine must resolve to a STOP, never to a
    silent pass: a baseline we cannot read is NOT a baseline of zero, and a
    workspace whose file discovery came up empty is NOT a workspace with zero
    swallows. The ratchet caller maps this to exit 2 (block), the same
    undetermined channel as check-test-weakening.py."""


def read_baseline(path: Path | None = None) -> int:
    """Read the pinned fail-open count from the baseline file.

    `path` defaults to BASELINE_FILE, bound at CALL time (not def time) so the
    module-level constant is the single source of truth even if it is reassigned.

    Format: exactly one non-negative integer on its own line; blank lines and
    `#`-comment lines are ignored. A missing file, no integer, more than one
    integer, or a negative / non-numeric value raises BaselineError so the
    caller fails closed. We deliberately do NOT default a missing or malformed
    baseline to zero — that would read "cannot determine the floor" as "the
    floor is zero, everything is a regression / nothing is", which is the very
    fail-open this scanner polices."""
    if path is None:
        path = BASELINE_FILE
    try:
        raw = path.read_text(encoding="utf-8")
    except FileNotFoundError as e:
        raise BaselineError(f"baseline file not found: {path}") from e
    except OSError as e:
        raise BaselineError(f"baseline file unreadable: {path}: {e}") from e
    nums = [s for s in (ln.strip() for ln in raw.splitlines())
            if s and not s.startswith("#")]
    if len(nums) != 1:
        raise BaselineError(
            f"baseline file {path} must contain exactly one integer line, "
            f"found {len(nums)}"
        )
    try:
        val = int(nums[0])
    except ValueError as e:
        raise BaselineError(f"baseline value is not an integer: {nums[0]!r}") from e
    if val < 0:
        raise BaselineError(f"baseline value is negative: {val}")
    return val


def all_crates_count() -> int:
    """Total un-allowlisted fail-open swallows across the whole workspace.

    Fails CLOSED on empty discovery (raises BaselineError): an empty `--all`
    target list means file discovery is broken, and reading that as a count of
    zero would let the ratchet report a spurious improvement-to-zero — the exact
    cannot-determine-as-clean this scanner exists to police."""
    files = iter_target_files(all_crates=True)
    if not files:
        raise BaselineError(
            "workspace discovery found ZERO files — cannot compute a trustworthy "
            "fail-open count (broken checkout / wrong cwd), refusing to fail open"
        )
    return sum(len(scan_file(path)) for path in files)


def ratchet_verdict(count: int, baseline: int) -> tuple[int, str]:
    """Pure ratchet comparison over an already-determined count and baseline.

    Returns (exit_code, message): 0 when the live count matches the baseline;
    1 on any drift — a REGRESSION (count above the baseline: a new swallow), or
    an unlocked IMPROVEMENT (count below the baseline: a gain that must be
    committed into the baseline so it cannot silently regress). The IO-layer
    cannot-determine cases (unreadable baseline / empty discovery) are exit 2 and
    are handled by the caller, not here."""
    if count > baseline:
        return 1, (
            f"fail-open-guard RATCHET: count ROSE {baseline} -> {count}. A new "
            f"fail-open swallow was introduced. Fix it (fail closed / capture the "
            f"rc), or, if it is a reviewed exception, add it to ALLOWLIST with a "
            f"reason. Raising the pinned baseline to admit it passes this script "
            f"but is caught in review — the pin is meant to move only DOWN."
        )
    if count < baseline:
        return 1, (
            f"fail-open-guard RATCHET: count FELL {baseline} -> {count} — an "
            f"improvement that is not yet locked in. Ratchet the baseline down so "
            f"the gain cannot silently regress: run `python3 "
            f"scripts/check-fail-open.py --update-baseline` and commit "
            f"scripts/check-fail-open.baseline."
        )
    return 0, (
        f"fail-open-guard RATCHET: count == baseline ({baseline}); no new "
        f"fail-open, burn-down floor held."
    )


def _write_baseline(count: int) -> None:
    BASELINE_FILE.write_text(
        "# fail-open-guard ratchet baseline. This is the pinned count of\n"
        "# un-allowlisted fail-open swallows across the whole workspace (the\n"
        "# `--all` scan). The `--ratchet` CI gate fails when the live count\n"
        "# drifts from this number in either direction. Lowering it locks in a\n"
        "# fix; RAISING it (to admit new swallows) still passes the script and is\n"
        "# caught only by review of this file's diff. Regenerate after a fix with:\n"
        "#   python3 scripts/check-fail-open.py --update-baseline\n"
        f"{count}\n",
        encoding="utf-8",
    )


def main(argv: list[str]) -> int:
    args = argv[1:]

    if "--update-baseline" in args:
        try:
            count = all_crates_count()
        except BaselineError as e:
            print(f"fail-open-guard: cannot compute count to pin: {e}",
                  file=sys.stderr)
            return 2
        _write_baseline(count)
        print(f"fail-open-guard: baseline pinned to {count} ({BASELINE_FILE}).")
        return 0

    if "--ratchet" in args:
        try:
            count = all_crates_count()
            baseline = read_baseline()
        except BaselineError as e:
            print(
                f"fail-open-guard RATCHET: cannot determine ({e}) — failing "
                f"closed (exit 2). A baseline or count we cannot trust is NOT a "
                f"pass.",
                file=sys.stderr,
            )
            return 2
        code, msg = ratchet_verdict(count, baseline)
        # On a regression, surface WHICH swallows exist so the new one is findable.
        if code == 1 and count > baseline:
            for path in iter_target_files(all_crates=True):
                for lineno, text, name in scan_file(path):
                    rel = path.relative_to(REPO) if REPO in path.parents else path
                    loc = str(lineno) if lineno != UNREADABLE else "?"
                    print(f"{rel}:{loc}: [{name}] {text.strip()}")
        print(msg, file=sys.stdout if code == 0 else sys.stderr)
        return code

    all_crates = "--all" in args
    explicit = [Path(a) for a in args if not a.startswith("-")]
    if explicit:
        files = explicit
        advisory = False
    else:
        files = iter_target_files(all_crates)
        advisory = all_crates  # --all is discovery-only, never merge-blocking
        # Fail CLOSED if discovery came up empty on the merge-blocking gate
        # surface. The GATE crate src/ dirs always hold tracked .rs files in a
        # real checkout, so an empty target list means file discovery is broken
        # (wrong cwd, `git ls-files` returned an empty set, glob mismatch) — NOT
        # that the surface is clean. Returning "clean" here would be this very
        # scanner committing the fail-open it polices: cannot-determine collapsed
        # into all-good. Refuse instead.
        if not files and not advisory:
            print(
                "fail-open-guard: gate surface discovery found ZERO files to "
                "scan — refusing to report clean (cannot-determine must fail "
                "closed). Check that this runs inside the repo checkout.",
                file=sys.stderr,
            )
            return 1
    total = 0
    advisory_class = 0
    for path in files:
        for lineno, text, name in scan_file(path):
            rel = path.relative_to(REPO) if REPO in path.parents else path
            loc = str(lineno) if lineno != UNREADABLE else "?"
            # Every hit is PRINTED, including the advisory class — it is scored
            # differently, never hidden. Only the blocking subset moves `total`
            # (and therefore the exit code); see ADVISORY_ONLY_PATTERNS.
            tag = " (advisory)" if name in ADVISORY_ONLY_PATTERNS else ""
            print(f"{rel}:{loc}: [{name}]{tag} {text.strip()}")
            if name in ADVISORY_ONLY_PATTERNS:
                advisory_class += 1
            else:
                total += 1
    if advisory_class:
        print(
            f"\nfail-open-guard: {advisory_class} hit(s) of the empty-collection "
            f"fallback class (b0cacd15). Scored on the --ratchet burn-down, NOT "
            f"on this verdict — see ADVISORY_ONLY_PATTERNS for why no ALLOWLIST "
            f"entry is the right answer here.",
            file=sys.stderr,
        )
    scope = "all crates (advisory)" if all_crates else "gate surface"
    if total:
        print(
            f"\nfail-open-guard: {total} swallow(s) on the {scope}. Each is a "
            f"cannot-determine collapsed into a clean/empty result. Fix by "
            f"failing closed (surface the error / capture the rc), or, if it is a "
            f"reviewed exception, add it to ALLOWLIST with a reason.",
            file=sys.stderr,
        )
        return 0 if advisory else 1
    print(f"fail-open-guard: {scope} clean (no fail-open swallow detected).")
    return 0


if __name__ == "__main__":
    raise SystemExit(main(sys.argv))
