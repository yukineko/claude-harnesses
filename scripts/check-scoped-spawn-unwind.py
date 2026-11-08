#!/usr/bin/env python3
"""Diff-scoped ratchet: refuse a commit that ADDS a `thread::scope` worker
without `catch_unwind` in a GATE crate's src (backlog 4620134f).

Why: `std::thread::scope` re-raises the panic of any auto-joined scoped worker
when the scope closes, so one worker's panic tears the whole gate down instead
of being folded into a failed slot (verified 2026-07-02; memory
thread-scope-reraises-panic). The rule — wrap every scoped worker body in
`std::panic::catch_unwind(std::panic::AssertUnwindSafe(..))` — used to be
applied by recall only. This scanner makes a NEW unwrapped spawn red.

The rule, exactly
-----------------
  1. GATE crate set G = the union of the `GATE_CRATES="..."` line of
     `scripts/rollout-plugins.sh` (the canonical source, see
     check-gate-crates-sync.py) read from the INDEX and from every judged
     parent. The union, so a commit that drops a crate from the list cannot
     also slip an unwrapped spawn into it. A side that holds the file but not
     exactly one such line, or no side holding the file at all, is
     UNDETERMINED.
  2. Judged parents P and the git plumbing (HEAD, or during a merge the
     independent heads of HEAD + MERGE_HEAD) are check-fail-open-diff.py's
     `recorded_parents` / `_blob`, reused verbatim so the two ratchets judge
     the same parent set with the same object-type checks.
  3. Candidates = the union over p in P of
     `git diff --cached --name-only -z --diff-filter=ACMRT p`, kept when the
     path is `crates/<g>/src/**.rs` for some g in G.
  4. For each candidate: after = number of unguarded scoped spawns in the
     staged (index) blob; before = MAX over p in P of that number in p's blob
     (absent = 0). Block when after > before for any candidate.

So a bare spawn that already exists at HEAD, carried unchanged through an
unrelated edit, is counted and NOT refused (the ratchet).

What "unguarded scoped spawn" means
-----------------------------------
The file is first lexed: comments, string literals (incl. raw / byte strings)
and char literals are blanked, so neither a `catch_unwind` in a comment nor a
`(` in a string changes the answer. Then:

  * a scope call is `thread::scope(` (any path prefix, whitespace allowed
    around `::`), or a bare `scope(` / `<alias>(` when the file imports
    `thread::scope` (`use std::thread::scope;`, `use std::thread::{self,
    scope as sc}`, ...);
  * inside the balanced argument span of each scope call, every
    `.spawn(` / `.spawn::<..>(` and every `spawn_scoped(` is a scoped spawn;
  * a scoped spawn is GUARDED iff its own balanced argument span contains the
    token `catch_unwind`. Anything else — including `s.spawn(worker)` where
    `worker` is a named fn whose body is elsewhere — is unguarded. That is
    the stricter reading: the guard must be visible at the spawn site.

Limits (what this does not see): a spawn whose scope call is not in the same
file text (e.g. `thread::scope(f)` with the spawning closure defined
elsewhere) is not counted; `catch_unwind` anywhere in the spawn's arguments
counts as guarded even if other code in the closure runs outside it; counts
are per file, so removing one unguarded spawn and adding another in the same
file nets out. A rename is judged as a new path (before = 0), which can only
block more, never less.

Exit codes: 0 no candidate's count rose / 1 a staged file adds an unguarded
scoped spawn / 2 cannot determine (git failure, no HEAD, unreadable
MERGE_HEAD, unparsable GATE_CRATES, an unterminated comment/string or
unbalanced parenthesis in a judged blob). Exit 2 blocks in the pre-commit
runner: a commit that could not be compared is not a clean commit.

Usage: python3 scripts/check-scoped-spawn-unwind.py   (from inside the repo)
"""

from __future__ import annotations

import importlib.util
import re
import subprocess
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent
_SPEC = importlib.util.spec_from_file_location(
    "check_fail_open_diff", _HERE / "check-fail-open-diff.py"
)
fod = importlib.util.module_from_spec(_SPEC)
# Same source-text exec as check-fail-open-diff.py uses for its own import
# (backlog 05726f9f: never run a stale .pyc of the subject).
exec(  # noqa: S102
    compile(_SPEC.loader.get_source(_SPEC.name), _SPEC.origin, "exec", dont_inherit=True),
    fod.__dict__,
)
Undetermined = fod.Undetermined

ROLLOUT = "scripts/rollout-plugins.sh"
_GATE_LINE = re.compile(r'^GATE_CRATES="([^"]*)"', re.M)
_RAW_OPEN = re.compile(r'(?:br|r)(#*)"')
_CHAR_LIT = re.compile(r"'(?:\\(?:x[0-9a-fA-F]{2}|u\{[0-9a-fA-F_]+\}|.)|[^'\\\n])'")


# ---------------------------------------------------------------- lexing


def sanitize(text: str) -> str:
    """`text` with comments, string literal contents and char literals
    replaced by spaces (newlines kept). Raises Undetermined on an
    unterminated block comment or string."""
    out = list(text)
    n = len(text)
    i = 0

    def blank(a: int, b: int) -> None:
        for k in range(a, b):
            if out[k] != "\n":
                out[k] = " "

    def ident(c: str) -> bool:
        return c.isalnum() or c == "_"

    while i < n:
        c = text[i]
        if text.startswith("//", i):
            j = text.find("\n", i)
            j = n if j < 0 else j
            blank(i, j)
            i = j
            continue
        if text.startswith("/*", i):
            depth, j = 1, i + 2
            while j < n and depth:
                if text.startswith("/*", j):
                    depth, j = depth + 1, j + 2
                elif text.startswith("*/", j):
                    depth, j = depth - 1, j + 2
                else:
                    j += 1
            if depth:
                raise Undetermined("unterminated block comment")
            blank(i, j)
            i = j
            continue
        prev = text[i - 1] if i else ""
        m = None
        if not ident(prev):
            m = _RAW_OPEN.match(text, i)
        if m:
            close = '"' + m.group(1)
            j = text.find(close, m.end())
            if j < 0:
                raise Undetermined("unterminated raw string literal")
            blank(i, j + len(close))
            i = j + len(close)
            continue
        if c == '"':
            j = i + 1
            while j < n and text[j] != '"':
                j += 2 if text[j] == "\\" else 1
            if j >= n:
                raise Undetermined("unterminated string literal")
            blank(i, j + 1)
            i = j + 1
            continue
        if c == "'":
            m = _CHAR_LIT.match(text, i)
            if m:
                blank(i, m.end())
                i = m.end()
                continue
        i += 1
    return "".join(out)


def _close_paren(s: str, open_at: int) -> int:
    """Index of the `)` matching the `(` at `open_at` in sanitized text."""
    depth = 0
    for k in range(open_at, len(s)):
        if s[k] == "(":
            depth += 1
        elif s[k] == ")":
            depth -= 1
            if depth == 0:
                return k
    raise Undetermined(f"unbalanced parenthesis opened at offset {open_at}")


_SCOPE_PATH = re.compile(r"\bthread\s*::\s*scope\s*\(")
_USE = re.compile(r"\buse\b[^;]*?\bthread\s*::\s*(?:\{([^}]*)\}|scope\b(?:\s+as\s+(\w+))?)", re.S)
_SPAWN = re.compile(r"\.\s*spawn\s*(?:::\s*<[^()]*>\s*)?\(|\bspawn_scoped\s*\(")


def _scope_names(s: str) -> set[str]:
    names: set[str] = set()
    for m in _USE.finditer(s):
        if m.group(1) is not None:
            for item in m.group(1).split(","):
                parts = item.split()
                if parts and parts[0] == "scope":
                    names.add(parts[2] if len(parts) == 3 and parts[1] == "as" else "scope")
        else:
            names.add(m.group(2) or "scope")
    return names


def unguarded_spawns(text: str) -> list[int]:
    """1-based line numbers of the unguarded scoped spawns in `text`."""
    s = sanitize(text)
    opens = [m.end() - 1 for m in _SCOPE_PATH.finditer(s)]
    for name in _scope_names(s):
        pat = re.compile(r"(?<![\w:.])" + re.escape(name) + r"\s*\(")
        opens += [m.end() - 1 for m in pat.finditer(s)]
    spawns: set[int] = set()
    for o in opens:
        end = _close_paren(s, o)
        for m in _SPAWN.finditer(s, o, end):
            spawns.add(m.end() - 1)
    bad = []
    for p in sorted(spawns):
        if "catch_unwind" not in s[p:_close_paren(s, p) + 1]:
            bad.append(s.count("\n", 0, p) + 1)
    return bad


# ---------------------------------------------------------------- git side


def gate_crates(repo: Path, parents: list[str]) -> set[str]:
    crates: set[str] = set()
    seen = 0
    for side in ["", *parents]:
        text = fod._blob(repo, side, ROLLOUT)
        if text is None:
            continue
        seen += 1
        found = _GATE_LINE.findall(text)
        if len(found) != 1 or not found[0].split():
            raise Undetermined(
                f"{ROLLOUT} at {side or 'the index'} has {len(found)} "
                f"GATE_CRATES=\"...\" lines (need exactly one, non-empty)")
        crates.update(found[0].split())
    if not seen:
        raise Undetermined(f"{ROLLOUT} is absent from the index and every judged "
                           f"parent — the GATE crate set is unknown")
    return crates


def in_scope(rel: str, crates: set[str]) -> bool:
    parts = rel.split("/")
    return (len(parts) >= 4 and parts[0] == "crates" and parts[1] in crates
            and parts[2] == "src" and rel.endswith(".rs"))


def _count(rel: str, side: str, text: str) -> list[int]:
    try:
        return unguarded_spawns(text)
    except Undetermined as e:
        raise Undetermined(f"{rel} at {side or 'the index'}: {e}") from e


def evaluate(repo: Path) -> list[tuple[str, int, list[int]]]:
    parents = fod.recorded_parents(repo)
    crates = gate_crates(repo, parents)
    seen: dict[str, None] = {}
    for p in parents:
        d = fod._git(repo, "diff", "--cached", "--name-only", "-z",
                     "--diff-filter=ACMRT", p)
        if d.returncode != 0:
            raise Undetermined(f"git diff --cached {p} failed (exit "
                               f"{d.returncode}): {d.stderr.strip()}")
        for path in d.stdout.split("\0"):
            if path and in_scope(path, crates):
                seen.setdefault(path, None)
    rises = []
    for rel in seen:
        after_text = fod._blob(repo, "", rel)
        if after_text is None:
            raise Undetermined(f"{rel} is listed as staged but has no index blob")
        before = 0
        for p in parents:
            t = fod._blob(repo, p, rel)
            if t is not None:
                before = max(before, len(_count(rel, p, t)))
        after = _count(rel, "", after_text)
        if len(after) > before:
            rises.append((rel, before, after))
    return rises


def main() -> int:
    try:
        top = subprocess.run(["git", "rev-parse", "--show-toplevel"],
                             capture_output=True, text=True)
    except OSError as e:
        print(f"scoped-spawn-unwind: UNDETERMINED — cannot run git: {e}", file=sys.stderr)
        return 2
    if top.returncode != 0 or not top.stdout.strip():
        print(f"scoped-spawn-unwind: UNDETERMINED — git rev-parse --show-toplevel "
              f"failed: {top.stderr.strip()}", file=sys.stderr)
        return 2
    try:
        rises = evaluate(Path(top.stdout.strip()))
    except Undetermined as e:
        print(f"scoped-spawn-unwind: UNDETERMINED — {e}. A commit that could not "
              f"be compared is not a clean commit; blocking (exit 2).", file=sys.stderr)
        return 2
    if not rises:
        print("scoped-spawn-unwind: no staged GATE-crate file adds an unguarded "
              "thread::scope spawn.")
        return 0
    for rel, before, after in rises:
        print(f"{rel}: unguarded scoped spawns {before} -> {len(after)} in this commit "
              f"(before = max over the judged parents)", file=sys.stderr)
        for ln in after:
            print(f"  {rel}:{ln}", file=sys.stderr)
    print(
        "\nscoped-spawn-unwind: this commit ADDS a `thread::scope` worker whose "
        "spawn call does not contain `catch_unwind`. std::thread::scope re-raises "
        "a scoped worker's panic when the scope closes, so one panicking worker "
        "takes the whole gate down. Wrap the worker body: "
        "`s.spawn(move || std::panic::catch_unwind(std::panic::AssertUnwindSafe("
        "|| work())))` and fold Err into a failed slot. Counts are per file; all "
        "current unguarded spawns are listed.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
