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
discovery aid for the burn-down, not a merge gate, so pre-existing swallows in
non-gate crates do not block unrelated work.

Usage:
  python3 scripts/check-fail-open.py            # gate surface, blocking (exit 1 on hit)
  python3 scripts/check-fail-open.py --all      # whole workspace, advisory (exit 0)
  python3 scripts/check-fail-open.py <file>...   # scan explicit files (blocking)
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent

# The crates whose gates guard the fleet (CLAUDE.md GATE_CRATES). A fail-open
# here is load-bearing, so these are the merge-blocking surface.
GATE_CRATES = [
    "blastguard",
    "propguard",
    "specguard",
    "stuckguard",
    "mutategate",
    "overwatch",
]

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
    # GENUINE fail-open, grandfathered so this detector can ship and block NEW
    # instances; tracked for a dedicated fix. The overwatch test-freshness .rs
    # walk drops an unreadable subtree exactly like specguard testaudit/gather
    # did before rounds #7/#11 — its fix (make the walk fail closed) is filed.
    {
        "path": "crates/overwatch/src/test_freshness.rs",
        "pattern": "readdir-let-else-swallow",
        "needle": "std::fs::read_dir(&dir)",
        "reason": "filed: backlog 50ad2c1e (make the .rs walk fail closed)",
    },
    {
        "path": "crates/overwatch/src/test_freshness.rs",
        "pattern": "readdir-flatten-swallow",
        "needle": "entries.flatten()",
        "reason": "filed: backlog 50ad2c1e (per-entry error must not be swallowed)",
    },
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


def main(argv: list[str]) -> int:
    args = argv[1:]
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
    for path in files:
        for lineno, text, name in scan_file(path):
            rel = path.relative_to(REPO) if REPO in path.parents else path
            loc = str(lineno) if lineno != UNREADABLE else "?"
            print(f"{rel}:{loc}: [{name}] {text.strip()}")
            total += 1
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
