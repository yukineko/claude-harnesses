#!/usr/bin/env python3
"""Block a commit whose `.backlog/*.toml` carries conflict markers or is not TOML.

Why this gate exists (backlog ece51b35)
---------------------------------------
Measured 2026-09-24: the merge-completion commit 0d68e5d4 recorded a
`.backlog/tasks.toml` containing `<<<<<<<` conflict markers, and it was
fast-forwarded onto main. Nothing stopped it: post-merge only listed it as an
UNGATED COMMIT. The store is then unreadable to every `backlog` call
("failed to parse <path>") until someone repairs it by hand (24a00986).

What it checks
--------------
Every `.backlog/*.toml` blob recorded in the git INDEX — the content the commit
will actually contain, not the working tree. Each must:

  1. contain no conflict-marker line (`<<<<<<< `, `=======`, `>>>>>>> `, `|||||||`
     at the start of a line), and
  2. parse as TOML (`tomllib`), and
  3. during a merge (MERGE_HEAD present), keep every task id that either side
     added since the merge-base: an id added on HEAD or on a MERGE_HEAD but
     absent from every indexed .backlog/*.toml means the resolution took one
     side wholesale and dropped the other side's tasks (backlog 68fb7237).

The whole store is checked, not just the staged diff: the index IS the commit,
and a broken store is broken whether or not this commit is the one that
touched it.

Exit codes (fail-closed: "cannot determine" resolves to the restricted side)
    0  every indexed .backlog/*.toml is clean TOML with no markers
    1  at least one file has markers, does not parse, or a merge dropped an id
    2  undetermined: git failed, a blob could not be read or parsed on a merge
       side, or tomllib is missing
"""
from __future__ import annotations

import re
import subprocess
import sys

MARKER_RE = re.compile(r"^(<{7} |>{7} |={7}$|\|{7} )", re.MULTILINE)


def git(*args: str) -> bytes:
    proc = subprocess.run(["git", *args], capture_output=True, timeout=60)
    if proc.returncode != 0:
        raise RuntimeError(
            f"git {' '.join(args)} exited {proc.returncode}: "
            f"{proc.stderr.decode(errors='replace').strip()}"
        )
    return proc.stdout


def task_ids(doc: dict) -> set[str]:
    ids = set()
    for value in doc.values():
        if isinstance(value, list):
            for item in value:
                if isinstance(item, dict) and isinstance(item.get("id"), str):
                    ids.add(item["id"])
    return ids


def rev_ids(tomllib, rev: str, paths: list[str]) -> set[str]:
    """Task ids across `paths` at commit `rev`; a path absent at `rev` adds none."""
    present = set(
        git("ls-tree", "-r", "-z", "--name-only", rev, "--", ".backlog").decode().split("\0")
    )
    ids = set()
    for path in paths:
        if path in present:
            ids |= task_ids(tomllib.loads(git("show", f"{rev}:{path}").decode("utf-8")))
    return ids


def merge_heads() -> list[str]:
    """The MERGE_HEAD commits, or [] when no merge is in progress."""
    probe = subprocess.run(
        ["git", "rev-parse", "-q", "--verify", "MERGE_HEAD"], capture_output=True, timeout=60
    )
    if probe.returncode != 0:
        return []
    path = git("rev-parse", "--git-path", "MERGE_HEAD").decode().strip()
    with open(path, encoding="utf-8") as f:
        return [line.strip() for line in f if line.strip()]


def dropped_ids(tomllib, paths: list[str], indexed: set[str]) -> list[str]:
    """Ids added since the merge-base on HEAD or a MERGE_HEAD but missing from the index."""
    heads = merge_heads()
    if not heads:
        return []
    sides = ["HEAD", *heads]
    base = git("merge-base", "--octopus", *sides).decode().strip()
    side_paths = sorted(set(paths) | {".backlog/tasks.toml", ".backlog/tasks.done.toml"})
    base_ids = rev_ids(tomllib, base, side_paths)
    out = []
    for side in sides:
        for tid in sorted(rev_ids(tomllib, side, side_paths) - base_ids - indexed):
            out.append(f"merge drops task id {tid} added on {side} since merge-base {base[:8]}")
    return out


def main() -> int:
    try:
        import tomllib
    except ImportError:
        print("backlog-store: tomllib unavailable (python < 3.11) — cannot determine", file=sys.stderr)
        return 2
    try:
        listing = git("ls-files", "-z", "--cached", "--", ".backlog")
    except (RuntimeError, OSError, subprocess.TimeoutExpired) as e:
        print(f"backlog-store: cannot list the index: {e}", file=sys.stderr)
        return 2
    paths = [p for p in listing.decode().split("\0") if p.endswith(".toml")]

    bad = []
    indexed_ids: set[str] = set()
    for path in paths:
        try:
            text = git("show", f":{path}").decode("utf-8")
        except (RuntimeError, OSError, subprocess.TimeoutExpired, UnicodeDecodeError) as e:
            print(f"backlog-store: cannot read the indexed {path}: {e}", file=sys.stderr)
            return 2
        m = MARKER_RE.search(text)
        if m:
            line = text.count("\n", 0, m.start()) + 1
            bad.append(f"{path}:{line}: conflict marker {m.group(0).strip()!r}")
            continue
        try:
            indexed_ids |= task_ids(tomllib.loads(text))
        except tomllib.TOMLDecodeError as e:
            bad.append(f"{path}: not valid TOML: {e}")

    if not bad:
        try:
            bad.extend(dropped_ids(tomllib, paths, indexed_ids))
        except (RuntimeError, OSError, subprocess.TimeoutExpired, UnicodeDecodeError,
                tomllib.TOMLDecodeError) as e:
            print(f"backlog-store: cannot compare the merge sides: {e}", file=sys.stderr)
            return 2

    if bad:
        print("backlog-store: the backlog store about to be committed is broken:", file=sys.stderr)
        for b in bad:
            print(f"  {b}", file=sys.stderr)
        print(
            "  Resolve the merge in the store (or re-run `backlog merge-driver`) before committing.",
            file=sys.stderr,
        )
        return 1
    print(f"backlog-store: {len(paths)} indexed .backlog/*.toml file(s) clean.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
