#!/usr/bin/env python3
"""Pin constants that are hand-copied from one crate into another.

# Why this exists

A crate cannot always link the constant it depends on. `backlog` ships no
library target, so `backlog::lock::LOCK_STALE_TTL_SECS` — the TTL the *writer*
of the driver registry reaps by — cannot be `use`d from `condukt`, which reads
those same records and must agree about when one has aged out. The value is
therefore hand-copied, and until this gate existed the only thing linking the
two was a sentence in a docstring **in the copying crate**. Nobody editing
`crates/backlog/src/lock.rs` had any signal that a constant in another crate
had to move with it (measured 2026-09-08, backlog `6e30b6fc`: grep over
`crates/`, `scripts/` and every test found no assertion pinning the two).

CLAUDE.md section 6 is explicit that compliance must not rest on an
implementer reading prose. This is the mechanical half.

# Why a drift here is not symmetric

For the TTL entry below, the two directions are not equally bad:

  * condukt's copy **shorter** than backlog's (i.e. somebody raises backlog's)
    — condukt reads a record backlog still counts as live as aged out, resolves
    `Registrations::AllStaleHere`, and hands `Occupancy::Dead` to
    `is_removable` (delete) and `resume_verdict` (offer the directory to a
    second session). Both are irreversible-ish and both act on a worktree
    somebody is working in.
  * condukt's copy **longer** — it keeps a directory that could have been
    reclaimed. Restrictive; costs disk.

The dangerous direction is produced by the edit that *feels* safe: raising a
reap TTL reads as "be more patient before deleting things". That asymmetry is
the whole reason this is a gate and not a comment.

# Adding an entry

When you hand-copy a constant across a crate boundary, add it here rather than
writing a new one-off script. Give the regex a `(...)` group around the value
and anchor it hard enough that a rename cannot still match.

# Fail-closed

Every way of *not getting an answer* blocks, per CLAUDE.md section 3:

  * a cited file is missing,
  * a pattern matches zero times (the constant was renamed or restructured),
  * a pattern matches more than once (ambiguous — this script will not guess
    which one the other crate agreed with).

A renamed constant must not read as "no drift found". Silence here would be
exactly the fail-open this gate replaces.
"""

from __future__ import annotations

import re
import subprocess
import sys
from pathlib import Path

# Each entry: one canonical declaration, one or more hand-copies that must
# carry the identical value. `why` is printed on failure so the person who
# tripped it does not have to reverse-engineer the coupling.
PINS = [
    {
        "name": "driver-registry stale TTL",
        "canonical": (
            "crates/backlog/src/lock.rs",
            r"^pub\(crate\) const LOCK_STALE_TTL_SECS: i64 = (\d+);",
        ),
        "copies": [
            (
                "crates/condukt/src/wt_reconcile.rs",
                r"^const DRIVER_STALE_TTL_SECS: i64 = (\d+);",
            ),
        ],
        "why": (
            "backlog writes and reaps ~/.backlog/drivers/**.driver by this TTL; "
            "condukt reads the same records to decide whether a worktree's "
            "session is still alive. `backlog` has no lib target, so the value "
            "cannot be linked. If condukt's copy is the SHORTER one, it calls a "
            "live session's worktree dead and both is_removable (delete) and "
            "resume_verdict (hand it to a second session) act on that."
        ),
    },
]


def repo_root() -> Path:
    out = subprocess.run(
        ["git", "rev-parse", "--show-toplevel"],
        capture_output=True,
        text=True,
        check=False,
    )
    if out.returncode != 0:
        print(
            "check-cross-crate-constants: not inside a git repository "
            f"({out.stderr.strip()})",
            file=sys.stderr,
        )
        sys.exit(1)
    return Path(out.stdout.strip())


def read_value(root: Path, rel: str, pattern: str) -> tuple[str | None, str | None]:
    """Return (value, error). Exactly one of them is None."""
    path = root / rel
    try:
        text = path.read_text(encoding="utf-8")
    except OSError as e:
        return None, f"{rel} could not be read ({e})"
    hits = re.findall(pattern, text, re.MULTILINE)
    if len(hits) == 0:
        return None, (
            f"{rel} has no declaration matching /{pattern}/ — the constant was "
            f"renamed or restructured, so this script cannot tell whether the "
            f"copies still agree"
        )
    if len(hits) > 1:
        return None, (
            f"{rel} has {len(hits)} declarations matching /{pattern}/ "
            f"({', '.join(hits)}) — ambiguous, refusing to guess which one the "
            f"other crate was copied from"
        )
    return hits[0], None


def main() -> int:
    root = repo_root()
    failures: list[str] = []
    checked = 0

    for pin in PINS:
        name = pin["name"]
        crel, cpat = pin["canonical"]
        canonical, err = read_value(root, crel, cpat)
        if err is not None:
            failures.append(f"[{name}] canonical: {err}")
            continue

        for rel, pat in pin["copies"]:
            checked += 1
            value, err = read_value(root, rel, pat)
            if err is not None:
                failures.append(f"[{name}] copy: {err}")
                continue
            if value != canonical:
                failures.append(
                    f"[{name}] {rel} says {value}, but {crel} (canonical) says "
                    f"{canonical}\n    why: {pin['why']}"
                )

    if failures:
        print("check-cross-crate-constants: BLOCK", file=sys.stderr)
        for f in failures:
            print(f"  {f}", file=sys.stderr)
        print(
            "\n  Fix: make the copy equal the canonical declaration, or — if the "
            "coupling\n  genuinely changed — update the entry in "
            "scripts/check-cross-crate-constants.py\n  in the same commit.",
            file=sys.stderr,
        )
        return 1

    print(
        f"check-cross-crate-constants: {checked} hand-copied constant(s) agree "
        f"with their canonical declaration."
    )
    return 0


if __name__ == "__main__":
    sys.exit(main())
