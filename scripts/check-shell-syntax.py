#!/usr/bin/env python3
"""pre-commit gate: every tracked shell script must PARSE under /bin/bash.

Why this exists, measured rather than supposed
----------------------------------------------
On 2026-08-04 commit a2c1c972 added an explanatory comment inside this heredoc
in scripts/rollout-plugins.sh:

    all_names="$(python3 - "$MARKETPLACE" <<'PY'
    ...
    # every plugin NAME downstream -- e.g. `in_only()`'s exact-match case pattern
    ...
    PY
    )"

That heredoc lives inside a `$( )` command substitution, and bash 3.2 — the
/bin/bash macOS still ships — scans `$( )` WITHOUT skipping heredoc bodies. The
single apostrophe in `in_only()'s` therefore opened a quote that never closed,
and the ENTIRE file stopped parsing. The failure was reported ~770 lines away
(at the next `$'\t'`), so it read as unrelated to the change that caused it.

The consequence was not cosmetic: rollout-plugins.sh is the only supported way
to deploy this repo's plugins, so for a full day NOTHING could be rolled out.
The same pull that broke it also mass-bumped every plugin, and the fleet drifted
to 40 undeployed plugins out of 41 — including a fail-open fix that stayed dead
in the tree while the live harness kept running the defect.

Nobody noticed because the breakage is invisible to every gate the repo had:
the script is not Rust, so fmt/clippy/tests never look at it, and a script that
is never *executed* by CI is never parsed either. `bash -n` is the cheapest
possible observation of the property, and it was simply not being made.

CLAUDE.md 6 is explicit that the answer to "an editor will be careful" is a
deterministic machine check, not a note asking for care. This is that check.

What it checks
--------------
Every file tracked by git that is a shell script (a `.sh` name, or a
`#!...sh`-family shebang) is handed to `bash -n`, which parses without
executing. Nothing here runs the scripts.

The interpreter is `/bin/bash` DELIBERATELY, not `bash` from PATH. The point is
to parse against the oldest bash a contributor on this platform will actually
run — macOS pins /bin/bash at 3.2.57 (2007) while Homebrew's PATH bash is 5.x,
and 5.x parses the construct above just fine. Checking the newer one would have
declared the broken file clean.

Fail-closed (CLAUDE.md 3)
-------------------------
"Cannot determine" is never "clean":

  - git cannot list files            -> exit 2 (undetermined, blocks)
  - /bin/bash is missing/unusable    -> exit 2 (undetermined, blocks)
  - a listed file cannot be read     -> exit 2 (undetermined, blocks)
  - the file set comes back EMPTY    -> exit 2 (undetermined, blocks)

The empty case is called out because it is this repo's recurring shape: an
empty collection read downstream as "nothing to inspect, therefore fine". This
repo has never had zero shell scripts, so zero means the listing failed, not
that the surface is clean.

Exit codes: 0 = all parse, 1 = at least one does not, 2 = undetermined.
"""

from __future__ import annotations

import os
import subprocess
import sys

# The oldest bash a contributor on this platform actually invokes. See the
# module docstring: checking a newer PATH bash would have passed the file that
# broke the fleet.
BASH = "/bin/bash"

SHELL_SUFFIXES = (".sh", ".bash")

# A shebang naming any of these means the file is parsed by a bourne-family
# shell, so `bash -n` is a meaningful (and conservative) syntax check for it.
SHEBANG_SHELLS = ("bash", "/bin/sh", "/usr/bin/sh", "zsh", "dash", "ksh")


def undetermined(reason: str) -> "int":
    print(f"shell-syntax: UNDETERMINED — {reason}", file=sys.stderr)
    print(
        "  This blocks. A shell surface that could not be parsed is not a "
        "shell surface that parses.",
        file=sys.stderr,
    )
    return 2


def repo_root() -> "str | None":
    try:
        out = subprocess.run(
            ["git", "rev-parse", "--show-toplevel"],
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    root = out.stdout.strip()
    return root or None


def tracked_files(root: str) -> "list[str] | None":
    try:
        out = subprocess.run(
            ["git", "ls-files", "-z"],
            capture_output=True,
            text=True,
            cwd=root,
            timeout=60,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    return [p for p in out.stdout.split("\0") if p]


def is_shell_script(path: str) -> "bool | None":
    """True/False, or None when the file could not be inspected.

    None is a third answer on purpose: "I could not read this file" must not
    collapse into "this file is not a shell script", which would silently
    shrink the checked set.
    """
    if path.endswith(SHELL_SUFFIXES):
        return True
    try:
        with open(path, "rb") as fh:
            first = fh.readline(256)
    except FileNotFoundError:
        # Tracked but not present in the working tree (e.g. a sparse checkout).
        # Nothing to parse and nothing hidden — not an inspection failure.
        return False
    except OSError:
        return None
    if not first.startswith(b"#!"):
        return False
    try:
        line = first.decode("utf-8", errors="strict").strip()
    except UnicodeDecodeError:
        return None
    return any(sh in line for sh in SHEBANG_SHELLS)


def main() -> int:
    root = repo_root()
    if root is None:
        return undetermined("git could not report the repository root")

    if not os.access(BASH, os.X_OK):
        return undetermined(f"{BASH} is missing or not executable")

    files = tracked_files(root)
    if files is None:
        return undetermined("git ls-files failed")

    scripts = []
    for rel in files:
        verdict = is_shell_script(os.path.join(root, rel))
        if verdict is None:
            return undetermined(f"could not inspect {rel}")
        if verdict:
            scripts.append(rel)

    if not scripts:
        return undetermined(
            "no shell scripts found — this repo has never had zero, so the "
            "listing failed rather than the surface being clean"
        )

    broken = []
    for rel in scripts:
        try:
            proc = subprocess.run(
                [BASH, "-n", rel],
                capture_output=True,
                text=True,
                cwd=root,
                timeout=60,
            )
        except (OSError, subprocess.SubprocessError) as exc:
            return undetermined(f"{BASH} -n {rel} could not be run: {exc}")
        if proc.returncode != 0:
            broken.append((rel, proc.stderr.strip()))

    if broken:
        print("", file=sys.stderr)
        print(
            f"shell-syntax: {len(broken)} tracked shell script(s) do not parse "
            f"under {BASH}:",
            file=sys.stderr,
        )
        for rel, err in broken:
            print(f"  {rel}", file=sys.stderr)
            for line in err.splitlines():
                print(f"    {line}", file=sys.stderr)
        print("", file=sys.stderr)
        print(
            "  A script that does not parse never runs — and if it is a "
            "deploy or gate script, nothing it guards is being checked.",
            file=sys.stderr,
        )
        print(
            "  Watch for a lone apostrophe inside a heredoc that sits inside "
            "$( ): bash 3.2 does not skip heredoc bodies when scanning $( ), "
            "so one unbalanced quote breaks the whole file and is reported "
            "far from its cause.",
            file=sys.stderr,
        )
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
