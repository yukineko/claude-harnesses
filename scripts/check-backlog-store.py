#!/usr/bin/env python3
"""Block a commit whose `.backlog/*.toml` store is broken or loses a merge side's changes.

Why this gate exists
--------------------
backlog ece51b35 — measured 2026-09-24: the merge-completion commit 0d68e5d4
recorded a `.backlog/tasks.toml` containing `<<<<<<<` conflict markers, and it
was fast-forwarded onto main. The store is then unreadable to every `backlog`
call until someone repairs it by hand (24a00986).

backlog 68fb7237 — a conflicted merge resolved by taking one side wholesale
dropped the other side's newly added task ids.

backlog 8231b822 — when the task-store merge driver cannot run (e.g. a bare
`backlog merge-driver %O %A %B` with `backlog` not on PATH), git marks the store
unmerged but leaves OUR side verbatim with NO conflict markers. It parses, so a
plain `git add` + commit silently discards the other side's edits, moves
(tasks.toml -> tasks.done.toml) and deletions.

What it checks
--------------
Every `.backlog/*.toml` blob recorded in the git INDEX — the content the commit
will actually contain, not the working tree. Each must:

  1. contain no conflict-marker line (`<<<<<<< `, `=======`, `>>>>>>> `, `|||||||`
     at the start of a line), and
  2. parse as TOML (`tomllib`).

Then, across all indexed files:

  3. a task id may appear at most once (the same id in tasks.toml and
     tasks.done.toml, or twice in one file, is refused).
  4. during a merge (a MERGE_HEAD file exists), every one-sided change survives.
     Task records are collected per (file, id) for the octopus merge-base, for
     each parent (HEAD and every MERGE_HEAD) and for the index. For each parent P
     and each id whose records on P differ from the base (added, edited, removed,
     or moved between files): if no OTHER parent also changed that id, the
     index must hold exactly P's records for it (same file, same parsed record,
     or absent if P removed it). An id changed on two or more parents is a
     two-sided change, and whatever the index holds for it is accepted as a hand
     resolution. Records are compared as parsed TOML values, not as text.

The whole store is checked, not just the staged diff: the index IS the commit,
and a broken store is broken whether or not this commit is the one that
touched it.

Exit codes (fail-closed: "cannot determine" resolves to the restricted side)
    0  the indexed store is clean and (during a merge) keeps every one-sided change
    1  markers, unparseable TOML, a duplicate id, or a lost one-sided merge change
    2  undetermined: git failed; an indexed blob is not UTF-8; the merge-base's or
       a parent's store does not parse; the MERGE_HEAD file exists but is
       unreadable, empty, or names no commit; no merge-base exists (unrelated
       histories); or tomllib is missing
"""
from __future__ import annotations

import json
import os
import re
import subprocess
import sys

MARKER_RE = re.compile(r"^(<{7} |>{7} |={7}$|\|{7} )", re.MULTILINE)


class Undetermined(Exception):
    """The gate cannot judge; main() resolves it to exit 2."""


def git(*args: str) -> bytes:
    try:
        proc = subprocess.run(["git", *args], capture_output=True, timeout=60)
    except (OSError, subprocess.TimeoutExpired) as e:
        raise Undetermined(f"git {' '.join(args)}: {e}") from e
    if proc.returncode != 0:
        raise Undetermined(
            f"git {' '.join(args)} exited {proc.returncode}: "
            f"{proc.stderr.decode(errors='replace').strip()}"
        )
    return proc.stdout


def records(doc: dict, path: str) -> list[tuple[str, str, str]]:
    """(id, path, canonical record) for every task-shaped table in `doc`."""
    out = []
    for value in doc.values():
        if isinstance(value, list):
            for item in value:
                if isinstance(item, dict) and isinstance(item.get("id"), str):
                    out.append((item["id"], path, json.dumps(item, sort_keys=True, default=str)))
    return out


def by_id(recs: list[tuple[str, str, str]]) -> dict[str, tuple]:
    """id -> sorted tuple of (path, canonical record); an absent id is ()."""
    grouped: dict[str, list] = {}
    for tid, path, canon in recs:
        grouped.setdefault(tid, []).append((path, canon))
    return {tid: tuple(sorted(v)) for tid, v in grouped.items()}


def rev_records(tomllib, rev: str) -> dict[str, tuple]:
    """Task records of every `.backlog/*.toml` at commit `rev`, keyed by id."""
    listing = git("ls-tree", "-r", "-z", "--name-only", rev, "--", ".backlog").decode()
    recs = []
    for path in listing.split("\0"):
        if not path.endswith(".toml"):
            continue
        text = git("show", f"{rev}:{path}")
        try:
            recs.extend(records(tomllib.loads(text.decode("utf-8")), path))
        except (UnicodeDecodeError, tomllib.TOMLDecodeError) as e:
            raise Undetermined(f"{rev[:12]}:{path} cannot be read as TOML: {e}") from e
    return by_id(recs)


def merge_heads() -> list[str] | None:
    """The MERGE_HEAD commits; None only when no MERGE_HEAD file exists.

    A MERGE_HEAD file that exists but is unreadable, empty, or names something
    that is not a commit is undetermined (raises), never "no merge".
    """
    path = git("rev-parse", "--git-path", "MERGE_HEAD").decode().strip()
    if not path:
        raise Undetermined("git rev-parse --git-path MERGE_HEAD printed nothing")
    if not os.path.lexists(path):
        return None
    try:
        with open(path, encoding="utf-8") as f:
            lines = [line.strip() for line in f if line.strip()]
    except (OSError, UnicodeDecodeError) as e:
        raise Undetermined(f"MERGE_HEAD exists but cannot be read: {e}") from e
    if not lines:
        raise Undetermined("MERGE_HEAD exists but names no commit")
    return [git("rev-parse", "-q", "--verify", f"{line}^{{commit}}").decode().strip()
            for line in lines]


def lost_changes(tomllib, indexed: dict[str, tuple]) -> list[str]:
    """One-sided merge changes the index does not carry ([] when not merging)."""
    heads = merge_heads()
    if heads is None:
        return []
    parents = [git("rev-parse", "-q", "--verify", "HEAD^{commit}").decode().strip(), *heads]
    names = ["HEAD", *(f"MERGE_HEAD {h[:8]}" for h in heads)]
    base = git("merge-base", "--octopus", *parents).decode().strip()
    if not base:
        raise Undetermined("no merge-base for the merge parents")
    base_recs = rev_records(tomllib, base)
    parent_recs = [rev_records(tomllib, p) for p in parents]
    changed = [
        {tid for tid in set(base_recs) | set(recs) if recs.get(tid, ()) != base_recs.get(tid, ())}
        for recs in parent_recs
    ]
    out = []
    for i, recs in enumerate(parent_recs):
        for tid in sorted(changed[i]):
            if any(tid in changed[j] for j in range(len(parents)) if j != i):
                continue  # two-sided change: the index holds a hand resolution
            want = recs.get(tid, ())
            got = indexed.get(tid, ())
            if got != want:
                want_s = ", ".join(p for p, _ in want) or "absent"
                got_s = ", ".join(p for p, _ in got) or "absent"
                detail = " (record differs)" if want_s == got_s else ""
                out.append(
                    f"merge loses {names[i]}'s change to task {tid} since merge-base "
                    f"{base[:8]}: {names[i]} has it in [{want_s}], the index in [{got_s}]{detail}"
                )
    return out


def main() -> int:
    try:
        import tomllib
    except ImportError:
        print("backlog-store: tomllib unavailable (python < 3.11) — cannot determine", file=sys.stderr)
        return 2
    try:
        listing = git("ls-files", "-z", "--cached", "--", ".backlog")
        # An unmerged path is listed once per stage; dedupe, keep order.
        paths = list(dict.fromkeys(p for p in listing.decode().split("\0") if p.endswith(".toml")))

        bad = []
        recs = []
        for path in paths:
            try:
                text = git("show", f":{path}").decode("utf-8")
            except UnicodeDecodeError as e:
                raise Undetermined(f"cannot read the indexed {path}: {e}") from e
            m = MARKER_RE.search(text)
            if m:
                line = text.count("\n", 0, m.start()) + 1
                bad.append(f"{path}:{line}: conflict marker {m.group(0).strip()!r}")
                continue
            try:
                recs.extend(records(tomllib.loads(text), path))
            except tomllib.TOMLDecodeError as e:
                bad.append(f"{path}: not valid TOML: {e}")

        indexed = by_id(recs)
        for tid, where in sorted(indexed.items()):
            if len(where) > 1:
                bad.append(f"task id {tid} appears {len(where)} times in the index: "
                           + ", ".join(p for p, _ in where))

        if not bad:
            bad.extend(lost_changes(tomllib, indexed))
    except Undetermined as e:
        print(f"backlog-store: cannot determine: {e}", file=sys.stderr)
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
