#!/usr/bin/env python3
"""Diff-scoped ratchet: refuse a commit that ADDS a never-executed construct
(`if false {`, `while false {`, an `if false &&` / `if false ||` guard) to any
crate's src (backlog 32ac2ceb).

Why: user ruling 2026-11-07 — 「絶対に処理されないコードはプログラムとコンパイラの
ゴミ」. A block behind a literal `false` is never run; it reads like live logic
to a reviewer, it keeps code compiling that no test can reach, and it is the
classic way to "disable" a check while leaving it looking present
(memory fail-open-review-checklist §7). This scanner makes a NEW one red.

The rule, exactly
-----------------
  1. Judged parents P and the git plumbing (HEAD, or during a merge the
     independent heads of HEAD + MERGE_HEAD) are check-fail-open-diff.py's
     `recorded_parents` / `_blob` / `_git`, reused so every diff ratchet
     judges the same parent set with the same object-type checks.
  2. Candidates = the union over p in P of
     `git diff --cached --name-only -z --diff-filter=ACMRT p`, kept when the
     path is `crates/<any>/src/**.rs`.
  3. For each candidate: after = number of never-executed constructs in the
     staged (index) blob; before = MAX over p in P of that number in p's blob
     (absent = 0). Block when after > before for any candidate.

So an occurrence that already exists at HEAD, carried unchanged through an
unrelated edit, is counted and NOT refused (the ratchet).

What "never-executed construct" means
-------------------------------------
The file is first lexed with check-scoped-spawn-unwind.py's `sanitize`
(comments, string literals incl. raw / byte strings, and char literals are
blanked), so an `if false {` inside a comment, doc comment or string does not
count. Then every match of

    (if|while) [(] false [)] followed by `{`, `&&` or `||`

(keyword on a word boundary, whitespace incl. newlines allowed between the
tokens, the parentheses optional) is one construct. `else if false {` is
covered (it contains `if false {`). `if false || x` is counted too: the
`false ||` operand is dead text of the same class, and the guard reads as if
it gated something.

Limits (what this does not see): conditions that are constant only after
evaluation (`if !true`, `if 1 == 2`, `if FALSE_CONST`, `cfg!(any())`),
`false` reached through a macro argument, and `#[cfg(any())]` items. Counts
are per file, so removing one construct and adding another in the same file
nets out. A rename is judged as a new path (before = 0), which can only block
more, never less.

Exit codes: 0 no candidate's count rose / 1 a staged file adds a
never-executed construct / 2 cannot determine (git failure, no HEAD,
unreadable MERGE_HEAD, an unterminated comment/string in a judged blob).
Exit 2 blocks in the pre-commit runner: a commit that could not be compared
is not a clean commit.

Usage: python3 scripts/check-never-executed.py   (from inside the repo)
"""

from __future__ import annotations

import importlib.util
import re
import subprocess
import sys
from pathlib import Path

_HERE = Path(__file__).resolve().parent


def _load(name: str, file: str):
    spec = importlib.util.spec_from_file_location(name, _HERE / file)
    mod = importlib.util.module_from_spec(spec)
    # Source-text exec, as check-fail-open-diff.py / check-scoped-spawn-unwind.py
    # do for their own imports (backlog 05726f9f: never run a stale .pyc).
    exec(  # noqa: S102
        compile(spec.loader.get_source(spec.name), spec.origin, "exec", dont_inherit=True),
        mod.__dict__,
    )
    return mod


ssu = _load("check_scoped_spawn_unwind", "check-scoped-spawn-unwind.py")
fod = ssu.fod
Undetermined = fod.Undetermined
sanitize = ssu.sanitize

_DEAD = re.compile(r"\b(?:if|while)\s*\(?\s*false\s*\)?\s*(?:\{|&&|\|\|)")


def never_executed(text: str) -> list[int]:
    """1-based line numbers of the never-executed constructs in `text`."""
    s = sanitize(text)
    return [s.count("\n", 0, m.start()) + 1 for m in _DEAD.finditer(s)]


def in_scope(rel: str) -> bool:
    parts = rel.split("/")
    return (len(parts) >= 4 and parts[0] == "crates" and parts[1]
            and parts[2] == "src" and rel.endswith(".rs"))


def _count(rel: str, side: str, text: str) -> list[int]:
    try:
        return never_executed(text)
    except Undetermined as e:
        raise Undetermined(f"{rel} at {side or 'the index'}: {e}") from e


def evaluate(repo: Path) -> list[tuple[str, int, list[int]]]:
    parents = fod.recorded_parents(repo)
    seen: dict[str, None] = {}
    for p in parents:
        d = fod._git(repo, "diff", "--cached", "--name-only", "-z",
                     "--diff-filter=ACMRT", p)
        if d.returncode != 0:
            raise Undetermined(f"git diff --cached {p} failed (exit "
                               f"{d.returncode}): {d.stderr.strip()}")
        for path in d.stdout.split("\0"):
            if path and in_scope(path):
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
        print(f"never-executed: UNDETERMINED — cannot run git: {e}", file=sys.stderr)
        return 2
    if top.returncode != 0 or not top.stdout.strip():
        print(f"never-executed: UNDETERMINED — git rev-parse --show-toplevel "
              f"failed: {top.stderr.strip()}", file=sys.stderr)
        return 2
    try:
        rises = evaluate(Path(top.stdout.strip()))
    except Undetermined as e:
        print(f"never-executed: UNDETERMINED — {e}. A commit that could not "
              f"be compared is not a clean commit; blocking (exit 2).", file=sys.stderr)
        return 2
    if not rises:
        print("never-executed: no staged crate src file adds an `if false` / "
              "`while false` construct.")
        return 0
    for rel, before, after in rises:
        print(f"{rel}: never-executed constructs {before} -> {len(after)} in this "
              f"commit (before = max over the judged parents)", file=sys.stderr)
        for ln in after:
            print(f"  {rel}:{ln}", file=sys.stderr)
    print(
        "\nnever-executed: this commit ADDS code behind a literal `false` "
        "(`if false {`, `while false {`, `if false &&`/`||`). Code that can "
        "never run is garbage for the program and the compiler (user ruling "
        "2026-11-07), and a `false`-guarded check looks present while doing "
        "nothing. Delete the dead code, or make the condition real. Counts are "
        "per file; all current occurrences are listed.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    raise SystemExit(main())
