#!/usr/bin/env python3
"""Verify every plugin launcher is recorded EXECUTABLE in the git index.

Why this gate exists
--------------------
A plugin's hooks.json execs `crates/<crate>/bin/<name>` — a POSIX-sh dispatcher
that picks the per-platform binary. rollout-plugins.sh copies that file into the
plugin cache with `rsync -a`, which preserves mode, and the mode rsync copies is
whatever the checkout produced. So a launcher committed WITHOUT the executable
bit is deployed without it, every hook of that plugin dies with `Permission
denied`, and — because a hook that cannot start produces no finding — the gate
reports nothing at all. It goes dark, not red.

Measured (2026-08-04, backlog 8cb3bc22): of the 39 launchers under crates/*/bin/
(41 plugins, three of which are skills-only and ship no binary), exactly one —
`crates/taintguard/bin/taintguard` — was staged 100644 while the other 38 were
100755. It was found BY HAND, after the gate had been silently inert; nothing in
the repo could detect a 1-in-39 deviation. `check-plugin-rollout.py` was the
closest thing and it only asks whether the launcher FILE EXISTS (see
`_bin_launcher_problem`), never what mode it carries.

Why the git INDEX and not the working tree
------------------------------------------
This repo sets `core.fileMode=false` (it is worked on across a WSL/Windows
boundary, where the filesystem cannot represent the bit faithfully). With that
set, git ignores the working tree's mode entirely: `os.access(p, os.X_OK)` and
`stat().st_mode` describe the local filesystem, not what a fresh `git clone`
would produce, and on a drvfs mount they can read 0777 for a file the index
records as 100644. The index is the only place the answer lives, so the mode
compared here comes from `git ls-files -s` and nowhere else. A working-tree
check would have passed on the very machine where the defect was introduced.

Scope
-----
`crates/<crate>/bin/<name>` — one level under a crate's `bin/`, basename with no
`.` in it. The dotless rule keeps a documentation file that happens to live there
(`bin/README.md`) out of scope without needing an allowlist, and the
single-level rule excludes `crates/<crate>/src/bin/*.rs` (Rust bin TARGETS,
ordinary source, not launchers) which a naive `crates/*/bin/*` pathspec DOES
match, since git's `*` crosses `/`.

Per-platform binaries (`<name>-linux-x64`, `<name>-windows-x64.exe`, ...) are
build artifacts and are not tracked, so they never appear here. If one is ever
committed it is in scope too, and must be executable for the same reason.

Fail-closed contract
--------------------
  exit 0  every tracked launcher is 100755
  exit 1  at least one is not                        -> block
  exit 2  the verdict could not be determined        -> block

Exit 2 covers both "git could not be asked" and "the scan matched ZERO
launchers". The second is not a pass: this repo has 39 of them, so an empty
result means the scope broke (wrong cwd, a renamed layout, a `-s` that stopped
printing modes), and a gate whose scope has silently shrunk to nothing reports
clean most convincingly at the moment it stopped checking anything. Same
reasoning as check-raw-io-ratchet.py's zero-file guard.

Run from the repo root:  python3 scripts/check-launcher-exec-bit.py
"""
from __future__ import annotations

import os
import re
import subprocess
import sys

REPO = os.getcwd()

RC_OK = 0
RC_NOT_EXECUTABLE = 1
RC_UNDETERMINED = 2

EXPECTED_MODE = "100755"

# `bin/` one level under a crate, dotless basename. See the module docstring for
# why both halves are needed.
LAUNCHER_RE = re.compile(r"^crates/([^/]+)/bin/([^/.]+)$")

# `git ls-files -s` output: "<mode> <object> <stage>\t<path>".
LS_FILES_RE = re.compile(r"^(\d{6}) [0-9a-f]+ \d+\t(.*)$")

# Modes git can record, spelled out so the message can say what was found
# instead of printing a bare number. 100644 is the defect this gate was built
# for; the others are included because they are equally unable to be exec'd as a
# launcher and silently degrade the same way.
MODE_NAMES = {
    "100644": "regular file, NOT executable",
    "100755": "regular file, executable",
    "120000": "symlink",
    "160000": "gitlink (submodule)",
}


class Undetermined(Exception):
    """The verdict could not be reached. Never resolved to "clean"."""


def index_entries(repo=None):
    """Every git-index entry as (mode, path), or raise Undetermined.

    Reads the INDEX, not the worktree — see the module docstring. Uses -z so a
    path containing a newline cannot split one entry into two (git would quote
    such a path without -z, and the quoted form would not match LAUNCHER_RE,
    silently dropping it from scope).
    """
    repo = repo or REPO
    try:
        proc = subprocess.run(
            ["git", "-C", str(repo), "ls-files", "-s", "-z", "--", "crates"],
            capture_output=True,
            text=True,
        )
    except OSError as exc:
        raise Undetermined(f"could not run git ls-files: {exc}") from exc
    if proc.returncode != 0:
        raise Undetermined(
            f"git ls-files exited {proc.returncode}: {proc.stderr.strip() or '(no stderr)'}"
        )

    entries = []
    for record in proc.stdout.split("\0"):
        if not record:
            continue
        m = LS_FILES_RE.match(record)
        if not m:
            # An unparseable record means this function does not know what git
            # just said. Skipping it would drop a launcher out of scope without
            # a word, which is the failure mode this whole gate exists to close.
            raise Undetermined(f"unparseable `git ls-files -s -z` record: {record!r}")
        entries.append((m.group(1), m.group(2)))
    return entries


def launchers(entries):
    """The (mode, path) entries that are plugin launchers, sorted by path."""
    return sorted(
        (mode, path) for mode, path in entries if LAUNCHER_RE.match(path)
    )


def problems(found):
    """Human-readable findings for launchers whose index mode is not 100755."""
    out = []
    for mode, path in found:
        if mode == EXPECTED_MODE:
            continue
        what = MODE_NAMES.get(mode, "unrecognized mode")
        out.append(
            f"{path}: git index mode is {mode} ({what}), expected {EXPECTED_MODE}. "
            "hooks.json execs this path directly, so as deployed it fails with "
            "Permission denied and the plugin's hooks never run — the gate goes "
            "dark rather than red."
        )
    return out


def main():
    try:
        found = launchers(index_entries())
    except Undetermined as exc:
        print(f"launcher-exec-bit: UNDETERMINED — {exc}", file=sys.stderr)
        print(
            "  Blocking rather than assuming clean: an uninspected launcher set "
            "is not a clean one.",
            file=sys.stderr,
        )
        return RC_UNDETERMINED

    if not found:
        print(
            "launcher-exec-bit: UNDETERMINED — found ZERO tracked "
            "crates/<crate>/bin/<name> launchers.",
            file=sys.stderr,
        )
        print(
            "  This repo has 39 of them, so an empty scope means the scan broke "
            "(wrong cwd? renamed layout?), not that there is nothing to check.",
            file=sys.stderr,
        )
        return RC_UNDETERMINED

    found_problems = problems(found)
    if not found_problems:
        print(
            f"launcher-exec-bit: all {len(found)} plugin launcher(s) recorded "
            f"{EXPECTED_MODE} in the git index."
        )
        return RC_OK

    print(
        f"LAUNCHER NOT EXECUTABLE ({len(found_problems)} of {len(found)} launcher(s) checked):",
        file=sys.stderr,
    )
    for p in found_problems:
        print(f"  - {p}", file=sys.stderr)
    print(
        "\nFix (per path, then commit the index change):\n"
        "    git update-index --chmod=+x <path>\n"
        "`chmod +x` alone is NOT enough: this repo sets core.fileMode=false, so "
        "git ignores the working tree's mode and the index keeps the old one.",
        file=sys.stderr,
    )
    return RC_NOT_EXECUTABLE


if __name__ == "__main__":
    sys.exit(main())
