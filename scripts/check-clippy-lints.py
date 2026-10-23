#!/usr/bin/env python3
"""Gate: refuse a commit that weakens the workspace deny-lint CONFIGURATION.

This scanner does NOT run `cargo`, `cargo clippy` or `cargo fmt`. It reads
`Cargo.toml` manifests (working tree and HEAD) and nothing else. Executing
clippy on changed crates is owned by donegate's lint-changed check; an earlier
version of this script ran `cargo clippy -p <pkg> --all-targets` itself, which
duplicated that work on every commit, and was removed.
`scripts/tests/precommit-clippy-gate.sh` Part T pins the removal: a fake `cargo`
on PATH that writes a marker file must never be executed.

What it protects
----------------
The workspace root `Cargo.toml` declares

    [workspace.lints.clippy]
    unwrap_used = "deny"
    expect_used = "deny"
    panic       = "deny"

and a crate inherits them only by writing `[lints] workspace = true` in its own
manifest. Those three lints are the physical exits a verdict-bearing crate uses
to turn "cannot determine" into a crash. Whether clippy then enforces them is
the clippy runner's job; whether the CONFIGURATION that makes clippy enforce
them is still there is this scanner's job. A commit that quietly drops the
opt-in (or the root deny) would make every later clippy run pass vacuously.

It blocks (exit 1) when, comparing the working tree against HEAD:

  * a changed crate (any path under `crates/<dir>/` differs from HEAD, or is
    untracked) whose HEAD `crates/<dir>/Cargo.toml` had `[lints] workspace = true`
    no longer has it (removed, or set to anything other than boolean true);
  * the root `Cargo.toml` changed and `[workspace.lints.clippy]` no longer has
    a `deny` (or the stricter `forbid`) level for unwrap_used, expect_used or
    panic that HEAD had — removed, or downgraded to `warn`/`allow`. Both the
    string form (`panic = "deny"`) and the table form
    (`panic = { level = "deny", priority = 1 }`) are recognised.

Gaining an opt-in, adding a new crate, deleting a crate's manifest outright, or
editing a manifest while keeping the opt-in is not a violation (exit 0).

Diff source == scanned content
------------------------------
The changed set is taken from the WORKING TREE — `git diff --name-only HEAD`
unioned with `git ls-files --others --exclude-standard` — and NEVER from
`git diff --cached`. The manifests compared are the working-tree files. If the
selector read the index while the comparison reads the working tree, a clean
staged version with an unstaged opt-in removal would be selected as "nothing
changed". Part J of the test suite pins this.

Cannot-determine resolves to the restricted side (CLAUDE.md 3)
--------------------------------------------------------------
Every state where the answer cannot be established is exit 2, and
`.githooks/pre-commit`'s `run()` helper treats exit 2 as UNDETERMINED and
blocks: `git diff` / `git ls-files` / `git ls-tree` / `git show` exiting
non-zero (which includes an unborn branch with no HEAD), a working-tree or HEAD
`Cargo.toml` that cannot be read or parsed, and an opted-in crate whose
manifest declares a target path (`package.build`, `lib.path`, or `path` on
`[[bin]]`/`[[example]]`/`[[test]]`/`[[bench]]`) that resolves outside the
repository. That last case is retained from the clippy-running version: such a
file can never appear in `git diff`, so this gate cannot see it at all, and the
restricted answer is to say so rather than skip it.

"Could not determine" is a DIFFERENT state from "determined: nothing was
weakened". They are separate variants (`Undetermined` vs a list of findings),
never one empty list; the former is exit 2, the latter exit 0.

Every opted-in crate's working-tree manifest is read on each run (for the
repo-escape check), so an unparsable manifest anywhere under `crates/*/` is
exit 2 even if that crate did not change. Measured ~0.1s for all manifests in
this repo when the clippy-running version introduced the same read.

Exit codes
----------
    0   determined: no changed crate lost its opt-in and the root denies hold
    1   a crate lost `[lints] workspace = true`, or the root lost a deny
    2   UNDETERMINED — git, a manifest, or a target path could not be resolved
"""

from __future__ import annotations

import os
import posixpath
import subprocess
import sys
import tomllib
from dataclasses import dataclass

CRATES_PREFIX = "crates/"
ROOT_MANIFEST = "Cargo.toml"
DENIED_LINTS = ("unwrap_used", "expect_used", "panic")
DENY_LEVELS = ("deny", "forbid")


# --------------------------------------------------------------------------
# Tri-state. `Undetermined` and an empty findings list are distinct on purpose:
# the first says the question could not be answered, the second says it was
# answered and the answer is "nothing weakened".
# --------------------------------------------------------------------------
@dataclass(frozen=True)
class Undetermined:
    reason: str


@dataclass(frozen=True)
class Absent:
    """The path does not exist at HEAD (a determined answer, not a failure)."""


def _git(repo: str, *args: str) -> str | None:
    """Run git and return stdout, or None if it could not be trusted.

    None covers both a failed launch and a non-zero exit. The caller must turn
    None into Undetermined; it must never be flattened into an empty result.
    """
    try:
        proc = subprocess.run(
            ("git", "-C", repo, *args),
            capture_output=True,
            text=True,
            timeout=60,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None
    return proc.stdout


def _repo_root() -> str | None:
    try:
        proc = subprocess.run(
            ("git", "rev-parse", "--show-toplevel"),
            capture_output=True,
            text=True,
            timeout=30,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if proc.returncode != 0:
        return None
    root = proc.stdout.strip()
    return root or None


def changed_paths(repo: str) -> Undetermined | frozenset[str]:
    """Repo-relative paths that differ from HEAD in the WORKING TREE.

    The union of tracked modifications (`git diff --name-only HEAD`) and
    untracked-but-not-ignored files (`git ls-files --others --exclude-standard`).
    Not `git diff --cached`: see the module docstring's diff-source invariant.

    An unborn branch makes `git diff HEAD` fail and is reported as Undetermined
    rather than carved out.
    """
    diff = _git(repo, "diff", "--name-only", "HEAD", "--")
    if diff is None:
        return Undetermined(
            "`git diff --name-only HEAD` did not succeed, so the changed-file "
            "set is unknown (an unborn branch with no commits also lands here)"
        )

    untracked = _git(repo, "ls-files", "--others", "--exclude-standard")
    if untracked is None:
        return Undetermined(
            "`git ls-files --others --exclude-standard` did not succeed, so "
            "newly added files could not be enumerated"
        )

    paths = set()
    for blob in (diff, untracked):
        for line in blob.splitlines():
            line = line.strip()
            if line:
                paths.add(line)
    return frozenset(paths)


def _crate_dirs_with_manifests(repo: str) -> Undetermined | tuple[str, ...]:
    """Every `crates/<dir>` that has a working-tree Cargo.toml, sorted."""
    base = os.path.join(repo, "crates")
    try:
        entries = sorted(os.listdir(base))
    except FileNotFoundError:
        return ()
    except OSError as exc:
        return Undetermined(f"could not list {base}: {exc}")
    out = []
    for name in entries:
        if os.path.isfile(os.path.join(base, name, "Cargo.toml")):
            out.append(name)
    return tuple(out)


def _read_manifest(path: str) -> Undetermined | dict:
    try:
        with open(path, "rb") as handle:
            return tomllib.load(handle)
    except OSError as exc:
        return Undetermined(f"could not read {path}: {exc}")
    except tomllib.TOMLDecodeError as exc:
        return Undetermined(f"could not parse {path}: {exc}")


def _head_manifest(repo: str, rel: str) -> Undetermined | Absent | dict:
    """The manifest at `HEAD:<rel>`, parsed; Absent if HEAD has no such file.

    Existence is asked of `git ls-tree` first so that "not at HEAD" (a new
    crate — determined) is never confused with "`git show` failed"
    (Undetermined).
    """
    listing = _git(repo, "ls-tree", "--name-only", "HEAD", "--", rel)
    if listing is None:
        return Undetermined(
            f"`git ls-tree HEAD -- {rel}` did not succeed, so whether HEAD "
            f"has this manifest is unknown"
        )
    if not listing.strip():
        return Absent()
    text = _git(repo, "show", f"HEAD:{rel}")
    if text is None:
        return Undetermined(f"`git show HEAD:{rel}` did not succeed")
    try:
        return tomllib.loads(text)
    except tomllib.TOMLDecodeError as exc:
        return Undetermined(f"could not parse HEAD:{rel}: {exc}")


def _opts_in(manifest: dict) -> bool:
    """`[lints] workspace = true`, literally the boolean true."""
    lints = manifest.get("lints")
    if not isinstance(lints, dict):
        return False
    return lints.get("workspace") is True


def _clippy_level(manifest: dict, lint: str) -> str | None:
    """The level of `[workspace.lints.clippy] <lint>`, string or table form."""
    workspace = manifest.get("workspace")
    if not isinstance(workspace, dict):
        return None
    lints = workspace.get("lints")
    if not isinstance(lints, dict):
        return None
    clippy = lints.get("clippy")
    if not isinstance(clippy, dict):
        return None
    value = clippy.get(lint)
    if isinstance(value, str):
        return value
    if isinstance(value, dict):
        level = value.get("level")
        if isinstance(level, str):
            return level
    return None


def _manifest_source_paths(manifest: dict) -> tuple[str, ...]:
    """Every manifest value naming a target source file this package compiles.

    `build = true` / `build = false` select or disable the default `build.rs`;
    only a string names some other file.
    """
    found: list[str] = []

    package = manifest.get("package")
    if isinstance(package, dict):
        build = package.get("build")
        if isinstance(build, str) and build.strip():
            found.append(build.strip())

    lib = manifest.get("lib")
    if isinstance(lib, dict):
        path = lib.get("path")
        if isinstance(path, str) and path.strip():
            found.append(path.strip())

    for table in ("bin", "example", "test", "bench"):
        entries = manifest.get(table)
        if not isinstance(entries, list):
            continue
        for entry in entries:
            if not isinstance(entry, dict):
                continue
            path = entry.get("path")
            if isinstance(path, str) and path.strip():
                found.append(path.strip())

    return tuple(found)


def _repo_escape(manifest: dict, crate_dir: str) -> Undetermined | None:
    """Undetermined if any declared target path leaves the repository."""
    inside = f"{CRATES_PREFIX}{crate_dir}/"
    manifest_path = f"{inside}Cargo.toml"
    for raw in _manifest_source_paths(manifest):
        normalised = raw.replace("\\", "/")
        if posixpath.isabs(normalised):
            return Undetermined(
                f"{manifest_path} compiles {raw!r}, an absolute path; the gate "
                f"cannot see a file outside the working tree"
            )
        resolved = posixpath.normpath(posixpath.join(inside, normalised))
        if resolved == ".." or resolved.startswith("../"):
            return Undetermined(
                f"{manifest_path} compiles {raw!r}, which resolves to "
                f"{resolved!r} — outside the repository, so it can never "
                f"appear in `git diff` and is invisible to this gate"
            )
    return None


def scan(repo: str) -> Undetermined | list[str]:
    """Findings (human-readable), or Undetermined. Empty list = determined clean."""
    changed = changed_paths(repo)
    if isinstance(changed, Undetermined):
        return changed

    # Every opted-in crate's working-tree manifest: unreadable/unparsable or a
    # repo-escaping target path is Undetermined, regardless of what changed.
    all_dirs = _crate_dirs_with_manifests(repo)
    if isinstance(all_dirs, Undetermined):
        return all_dirs
    for crate_dir in all_dirs:
        manifest = _read_manifest(os.path.join(repo, "crates", crate_dir, "Cargo.toml"))
        if isinstance(manifest, Undetermined):
            return manifest
        if _opts_in(manifest):
            escape = _repo_escape(manifest, crate_dir)
            if escape is not None:
                return escape

    findings: list[str] = []

    dirs: set[str] = set()
    for path in changed:
        if not path.startswith(CRATES_PREFIX):
            continue
        head, sep, _ = path[len(CRATES_PREFIX) :].partition("/")
        if sep and head:
            dirs.add(head)

    for crate_dir in sorted(dirs):
        rel = f"{CRATES_PREFIX}{crate_dir}/Cargo.toml"
        before = _head_manifest(repo, rel)
        if isinstance(before, Undetermined):
            return before
        if isinstance(before, Absent) or not _opts_in(before):
            continue
        path = os.path.join(repo, rel)
        if not os.path.exists(path):
            print(f"  crate manifest removed (not a lint downgrade): {rel}")
            continue
        after = _read_manifest(path)
        if isinstance(after, Undetermined):
            return after
        if not _opts_in(after):
            findings.append(
                f"{rel}: `[lints] workspace = true` was present at HEAD and is "
                f"gone (or no longer boolean true) in the working tree"
            )

    if ROOT_MANIFEST in changed:
        before = _head_manifest(repo, ROOT_MANIFEST)
        if isinstance(before, Undetermined):
            return before
        if not isinstance(before, Absent):
            root_path = os.path.join(repo, ROOT_MANIFEST)
            after: dict | Undetermined
            if os.path.exists(root_path):
                after = _read_manifest(root_path)
            else:
                after = {}
            if isinstance(after, Undetermined):
                return after
            for lint in DENIED_LINTS:
                was = _clippy_level(before, lint)
                now = _clippy_level(after, lint)
                if was in DENY_LEVELS and now not in DENY_LEVELS:
                    findings.append(
                        f"{ROOT_MANIFEST}: [workspace.lints.clippy] {lint} was "
                        f"{was!r} at HEAD and is {now!r} in the working tree"
                    )

    return findings


UNDETERMINED_NOTE = """
clippy-lints: UNDETERMINED — {reason}.

This blocks (exit 2). "Could not check" is not "clean": an uninspected surface
carries no evidence either way, and recording it as a pass would make this gate
indistinguishable from one that never ran. Fix what stopped the scanner rather
than loosening it.
"""

VIOLATION_NOTE = """
clippy-lints: blocked — the workspace deny-lint configuration was weakened:
{findings}

`[lints] workspace = true` and the root `[workspace.lints.clippy]` denies for
unwrap_used / expect_used / panic are how a verdict-bearing crate is stopped from
turning "cannot determine" into a crash. Restore them; do not drop the opt-in to
make a clippy run pass.
"""


def main() -> int:
    repo = _repo_root()
    if repo is None:
        sys.stderr.write(
            UNDETERMINED_NOTE.format(
                reason="`git rev-parse --show-toplevel` did not answer, so there "
                "is no repository to inspect"
            )
        )
        return 2

    result = scan(repo)
    if isinstance(result, Undetermined):
        sys.stderr.write(UNDETERMINED_NOTE.format(reason=result.reason))
        return 2

    if result:
        sys.stderr.write(
            VIOLATION_NOTE.format(findings="\n".join(f"  - {f}" for f in result))
        )
        return 1

    print("clippy-lints: [lints] configuration intact (no cargo/clippy run)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
