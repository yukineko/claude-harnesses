#!/usr/bin/env python3
"""Pre-commit file-growth ratchet: no staged `.rs` / `.py` file may exceed
LIMIT (2000) lines, except files grandfathered in the baseline, which may not
exceed their own recorded cap.

Why: a file that keeps growing past ~2000 lines is where refactors stall and
review degrades. The existing oversized files are recorded in
`scripts/check-file-growth.baseline` with their size at the time the gate was
introduced; that number is a CEILING, not an allowance to grow back into. The
ratchet only turns one way:

  * a staged file listed in the baseline passes iff its line count <= its cap;
  * a staged file NOT listed passes iff its line count <= LIMIT;
  * the STAGED baseline may not raise any cap relative to the baseline at HEAD.
    A NEW entry (absent from HEAD's baseline, or no baseline at HEAD) whose
    cap > LIMIT is allowed only to grandfather a file that is already that big
    in HEAD's tree: its cap must be <= the line count of `git show HEAD:<path>`.
    It is a violation if the path is absent at HEAD, the cap exceeds the HEAD
    count, or HEAD is unborn. A new entry with cap <= LIMIT only tightens and
    is allowed. Lowering or deleting entries is fine. To get a file under the
    limit, split it, then lower (or delete) its entry.

Scope: only the INDEX matters. The files checked are the staged added /
modified / renamed / copied / type-changed paths ending in `.rs` or `.py`
(`git diff --cached --no-renames`), and their line count is taken from the
staged blob (`git show :<path>`), never the working tree. A final line without
a trailing newline counts as a line (unlike `wc -l`). Files not staged are not
checked: an untouched oversized file is not this commit's change.

Baseline format: one entry per line, `<max_lines> <path>`; blank lines and
lines starting with `#` are ignored. A malformed line or a duplicate path is
UNDETERMINED — the cap it was supposed to express is unknown.

Exit codes (fail-closed, CLAUDE.md §3):
  0  every staged source file is within its cap and the baseline did not loosen
  1  violation — each one is printed with its path, line count and cap
  2  undetermined — a git command failed (including reading HEAD:<path> for a
     grandfathered entry) or a baseline line is unparsable.
     Never 0: an uninspected commit is not a clean commit.

Run from anywhere inside the repository; it operates on the git toplevel of
its cwd.
"""

from __future__ import annotations

import subprocess
import sys

LIMIT = 2000
BASELINE_PATH = "scripts/check-file-growth.baseline"
EXTENSIONS = (".rs", ".py")


class Undetermined(Exception):
    """The verdict cannot be reached; the caller exits 2."""


def _git(root: str | None, *args: str) -> bytes:
    cmd = ["git"]
    if root is not None:
        cmd += ["-C", root]
    cmd += list(args)
    try:
        proc = subprocess.run(cmd, capture_output=True)
    except OSError as e:
        raise Undetermined(f"cannot run {' '.join(cmd)}: {e}") from e
    if proc.returncode != 0:
        err = proc.stderr.decode("utf-8", "replace").strip()
        raise Undetermined(
            f"`{' '.join(cmd)}` exited {proc.returncode}: {err or '(no stderr)'}"
        )
    return proc.stdout


def _toplevel() -> str:
    out = _git(None, "rev-parse", "--show-toplevel").decode("utf-8", "surrogateescape")
    top = out.strip()
    if not top:
        raise Undetermined("`git rev-parse --show-toplevel` printed nothing")
    return top


def _head_exists(root: str) -> bool:
    """True if HEAD resolves to a commit, False for an unborn HEAD.

    `rev-parse --verify -q` exits 1 silently for an unborn HEAD; any other
    failure shape (stderr output, other exit codes) is undetermined.
    """
    try:
        proc = subprocess.run(
            ["git", "-C", root, "rev-parse", "--verify", "-q", "HEAD^{commit}"],
            capture_output=True,
        )
    except OSError as e:
        raise Undetermined(f"cannot run git rev-parse HEAD: {e}") from e
    if proc.returncode == 0 and proc.stdout.strip():
        return True
    if proc.returncode == 1 and not proc.stdout.strip() and not proc.stderr.strip():
        # Confirm "unborn" positively rather than inferring it from silence:
        # HEAD must be a symbolic ref to a branch that has no commit yet.
        _git(root, "symbolic-ref", "-q", "HEAD")
        return False
    err = proc.stderr.decode("utf-8", "replace").strip()
    raise Undetermined(f"cannot resolve HEAD (exit {proc.returncode}): {err}")


def _line_count(blob: bytes) -> int:
    n = blob.count(b"\n")
    if blob and not blob.endswith(b"\n"):
        n += 1
    return n


def _parse_baseline(text: str, where: str) -> dict[str, int]:
    caps: dict[str, int] = {}
    for lineno, raw in enumerate(text.splitlines(), 1):
        line = raw.strip()
        if not line or line.startswith("#"):
            continue
        parts = line.split(None, 1)
        if len(parts) != 2 or not parts[0].isdigit() or not parts[0].isascii():
            raise Undetermined(
                f"{where}:{lineno}: unparsable baseline line {raw!r} "
                f"(expected `<max_lines> <path>`)"
            )
        cap, path = int(parts[0]), parts[1].strip()
        if path in caps:
            raise Undetermined(f"{where}:{lineno}: duplicate baseline entry for {path!r}")
        caps[path] = cap
    return caps


def _staged_baseline(root: str) -> dict[str, int]:
    listed = _git(root, "ls-files", "--cached", "-z", "--", BASELINE_PATH)
    if not listed.strip(b"\0"):
        return {}
    blob = _git(root, "show", f":{BASELINE_PATH}")
    return _parse_baseline(blob.decode("utf-8", "surrogateescape"), f"(index) {BASELINE_PATH}")


def _head_line_count(root: str, path: str) -> int | None:
    """Line count of `path` in HEAD's tree, or None if HEAD has no blob there.

    Absence is established positively via `ls-tree` (an exact path match with
    object type `blob`); any git failure raises Undetermined (exit 2), so an
    unreadable HEAD can never pass for "file is this big at HEAD".
    """
    listed = _git(root, "ls-tree", "-z", "HEAD", "--", path)
    for rec in listed.split(b"\0"):
        if not rec:
            continue
        meta, _, name = rec.partition(b"\t")
        if name.decode("utf-8", "surrogateescape") != path:
            continue
        fields = meta.split()
        if len(fields) != 3:
            raise Undetermined(f"unparsable `git ls-tree HEAD` record for {path!r}: {rec!r}")
        if fields[1] != b"blob":
            return None
        return _line_count(_git(root, "show", f"HEAD:{path}"))
    return None


def _head_baseline(root: str) -> dict[str, int]:
    if not _head_exists(root):
        return {}
    listed = _git(root, "ls-tree", "-z", "HEAD", "--", BASELINE_PATH)
    if not listed.strip(b"\0"):
        return {}
    blob = _git(root, "show", f"HEAD:{BASELINE_PATH}")
    return _parse_baseline(blob.decode("utf-8", "surrogateescape"), f"HEAD:{BASELINE_PATH}")


def _staged_sources(root: str) -> list[str]:
    out = _git(
        root, "diff", "--cached", "--no-renames", "--name-only", "-z",
        "--diff-filter=ACMRT",
    )
    paths = [p.decode("utf-8", "surrogateescape") for p in out.split(b"\0") if p]
    return sorted(p for p in paths if p.endswith(EXTENSIONS))


def check(root: str) -> list[str]:
    """Return the list of violation messages (empty = pass).

    Raises Undetermined when any input cannot be read or parsed.
    """
    violations: list[str] = []
    staged_caps = _staged_baseline(root)
    head_exists = _head_exists(root)
    head_caps = _head_baseline(root)

    for path, cap in sorted(staged_caps.items()):
        if path in head_caps:
            if cap > head_caps[path]:
                violations.append(
                    f"{BASELINE_PATH}: cap for {path} raised from "
                    f"{head_caps[path]} to {cap} (caps may only go down)"
                )
            continue
        if cap <= LIMIT:
            continue  # a new entry at or under the limit only tightens
        # A new entry over the limit grandfathers an ALREADY-oversized file:
        # allowed only up to that file's size in HEAD's tree, never growth.
        head_lines = _head_line_count(root, path) if head_exists else None
        if head_lines is None:
            where = "HEAD is unborn" if not head_exists else f"{path} does not exist at HEAD"
            violations.append(
                f"{BASELINE_PATH}: new entry {path} with cap {cap} exceeds the "
                f"{LIMIT}-line limit and {where} (only files already oversized "
                f"at HEAD can be grandfathered)"
            )
        elif cap > head_lines:
            violations.append(
                f"{BASELINE_PATH}: new entry {path} with cap {cap} exceeds its "
                f"{head_lines} lines at HEAD (grandfathering may not allow growth)"
            )

    for path in _staged_sources(root):
        lines = _line_count(_git(root, "show", f":{path}"))
        cap = staged_caps.get(path, LIMIT)
        if lines > cap:
            kind = "baseline cap" if path in staged_caps else "limit"
            violations.append(f"{path}: {lines} lines exceeds the {kind} of {cap}")
    return violations


def main() -> int:
    try:
        root = _toplevel()
        violations = check(root)
    except Undetermined as e:
        print(f"check-file-growth: UNDETERMINED — {e}", file=sys.stderr)
        return 2
    if not violations:
        return 0
    print("check-file-growth: staged source files over their line cap:", file=sys.stderr)
    for v in violations:
        print(f"  {v}", file=sys.stderr)
    print(
        f"  Split the file to get it under {LIMIT} lines (then lower or delete its\n"
        f"  entry in {BASELINE_PATH}). Caps in the baseline may only go down.",
        file=sys.stderr,
    )
    return 1


if __name__ == "__main__":
    sys.exit(main())
