#!/usr/bin/env python3
"""Gate: run `cargo clippy` on the crates that opted in to the workspace deny lints.

Why this exists
---------------
The workspace root `Cargo.toml` declares

    [workspace.lints.clippy]
    unwrap_used = "deny"
    expect_used = "deny"
    panic       = "deny"

and a crate inherits them only by writing `[lints] workspace = true` in its own
manifest. Those three lints are the physical exits a verdict-bearing crate uses
to turn "cannot determine" into a crash, so in a gate crate they must not
compile at all.

Before this scanner, `cargo clippy` ran in exactly ONE local place: donegate.toml,
as a Stop-hook check, workspace-wide, and only in a clone where `donegate trust`
has been run. `.githooks/pre-commit` and `.githooks/pre-push` invoke cargo
nowhere. So a commit could introduce a denied `.unwrap()` into an opted-in gate
crate and nothing at commit time would notice.

Diff source == scanned content (the invariant this scanner is built around)
--------------------------------------------------------------------------
The crate set is selected from the WORKING TREE — `git diff --name-only HEAD`
unioned with `git ls-files --others --exclude-standard` — and NEVER from
`git diff --cached`.

The reason is that clippy compiles the working tree. If the selector read the
index while the checker reads the working tree, the two disagree, and the
disagreement is exploitable in the permissive direction: stage a clean version of
a file, leave the violating change unstaged, and the selector finds no crate to
check. The gate then reports clean and the commit that git actually creates is
never inspected. Selecting from the same content that gets compiled is the only
arrangement where a pass means something. `scripts/tests/precommit-clippy-gate.sh`
Part J pins this by leaving a violation unstaged with an empty `--cached` diff and
requiring a block anyway.

The untracked half is not optional either: a brand-new `.rs` file in a gate crate
does not appear in `git diff HEAD` at all, so diff alone would miss a violation
in newly added code entirely. This mirrors the union already used by
`scripts/test-changed-crates.sh` (diff at :56, untracked at :70, unioned at :80).
Part K pins it.

Membership is DERIVED, never hardcoded
--------------------------------------
A crate is in scope iff its `Cargo.toml` contains a `[lints]` table with
`workspace = true`. There is no list of crate names in this file: a crate that
newly opts in is picked up on its next change with no edit here, and a crate
that opts out stops being checked. The cargo package name is then read from the
manifest's `[package]` section — deliberately NOT from `[[bin]]`/`[[bench]]`,
whose `name` fields can differ from the package name — because the directory
name is not always the package name. Same model as
`scripts/test-changed-crates.sh:83-106` (dir extraction at :83, `[package] name`
resolution at :92-106).

Verdict comes from the EXIT STATUS
----------------------------------
`cargo clippy`'s return code is the verdict. Its stdout/stderr are only ever
shown to the human. A checker that crashed, was killed, or could not be started
is not a checker that passed, and no amount of reassuring text on stdout changes
that.

Cannot-determine resolves to the restricted side (CLAUDE.md 3)
--------------------------------------------------------------
Every state where the crate set or the checker itself cannot be established is
exit 2, and `.githooks/pre-commit`'s `run()` helper treats exit 2 as UNDETERMINED
and blocks. Specifically: cargo not found, cargo found but not executable, cargo
failing to launch, `git diff` or `git ls-files` exiting non-zero (which includes
an unborn branch), and a `Cargo.toml` that cannot be read or parsed while
deciding membership.

Crucially, "could not determine the crate set" is a DIFFERENT state from
"git answered, and genuinely no opted-in crate changed". They are separate
variants below (`Undetermined` vs `Selection`), not one empty list, because an
empty list read as "nothing to check, therefore clean" is exactly the fail-open
CLAUDE.md 3 names. The former is exit 2, the latter is exit 0.

KNOWN LIMITATION — four verdict-bearing gate crates are NOT covered
-------------------------------------------------------------------
budgetguard, donegate, reviewgate and schemaguard have NO `[lints]` section at
all (measured 2026-07-28 on branch flow/clippy-precommit-gate: `grep -n
'^\\[lints\\]' crates/<name>/Cargo.toml` finds nothing in any of the four).
They instead hand-paste `#![deny(clippy::panic)]` into their crate roots
(crates/budgetguard/src/main.rs:6, crates/donegate/src/main.rs:9,
crates/reviewgate/src/main.rs:10, crates/schemaguard/src/lib.rs:6 and
crates/schemaguard/src/main.rs:3). `clippy::panic` is a different lint from
`clippy::unwrap_used` and `clippy::expect_used`, so those two are not denied in
those four crates by any invocation.

Since membership here is derived from the opt-in, this scanner does not check
them, and that is stated rather than left to be discovered. Closing it means
adding `[lints] workspace = true` to those four manifests, which touches
`crates/` and triggers this repo's plugin version-bump rules — deliberately out
of scope for the change that introduced this file, not a judgement that the gap
is unimportant.

A second, smaller limitation: selection is driven by paths under `crates/<dir>/`
plus the workspace root `Cargo.toml`. A change to some other file outside both
(for example a `build.rs` at the repository root, or a path dependency living
elsewhere) that alters what a gate crate compiles to would not select that crate.

Exit codes
----------
    0   checked (or nothing opted-in changed) and clippy was clean
    1   clippy reported a violation in at least one in-scope crate
    2   UNDETERMINED — the crate set or the checker could not be established
"""

from __future__ import annotations

import os
import shutil
import subprocess
import sys
import tomllib
from dataclasses import dataclass

# Test seam: an explicit cargo path, used by
# scripts/tests/precommit-clippy-gate.sh to exercise the not-executable and
# exit-status branches without a real toolchain. It is not a bypass — PATH is
# already an equivalent seam for any caller who can set environment variables,
# and neither can turn a non-zero clippy into a pass.
CARGO_ENV = "CHECK_CLIPPY_LINTS_CARGO"

CRATES_PREFIX = "crates/"
ROOT_MANIFEST = "Cargo.toml"


# --------------------------------------------------------------------------
# Tri-state. `Undetermined` and `Selection([])` are distinct on purpose: the
# first says the question could not be answered, the second says it was answered
# and the answer is "none". Collapsing them is the empty-set-reads-as-clean
# fail-open, so they are not the same type and cannot be confused by accident.
# --------------------------------------------------------------------------
@dataclass(frozen=True)
class Undetermined:
    reason: str


@dataclass(frozen=True)
class Selection:
    """Crates to check, as (package_name, crate_dir) pairs. May be empty, and an
    empty Selection means 'determined: nothing opted-in changed'."""

    crates: tuple[tuple[str, str], ...]


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
    rather than carved out. The carve-out may well be safe, but nothing here
    establishes that it is, and cannot-determine takes the restricted side.
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
    """Every `crates/<dir>` that has a Cargo.toml, sorted. Used when the
    workspace root manifest itself changed, since that can change the lint
    configuration every opted-in crate inherits."""
    base = os.path.join(repo, "crates")
    try:
        entries = sorted(os.listdir(base))
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


def _opts_in(manifest: dict) -> bool:
    """`[lints] workspace = true` — the sole membership criterion.

    `workspace` must literally be the boolean true; `"true"` or 1 are not it, and
    treating them as opt-in would be guessing at intent.
    """
    lints = manifest.get("lints")
    if not isinstance(lints, dict):
        return False
    return lints.get("workspace") is True


def _package_name(manifest: dict) -> str | None:
    """The `[package] name`. Read only from `[package]`: `[[bin]]`/`[[bench]]`
    entries also carry a `name`, and theirs can differ from the package name, so
    picking one of those up would hand `cargo -p` the wrong unit."""
    package = manifest.get("package")
    if not isinstance(package, dict):
        return None
    name = package.get("name")
    if isinstance(name, str) and name.strip():
        return name.strip()
    return None


def select_crates(repo: str) -> Undetermined | Selection:
    """Which cargo packages to run clippy on, derived from the working tree."""
    changed = changed_paths(repo)
    if isinstance(changed, Undetermined):
        return changed

    dirs: set[str] = set()
    for path in changed:
        if not path.startswith(CRATES_PREFIX):
            continue
        rest = path[len(CRATES_PREFIX) :]
        head, sep, _ = rest.partition("/")
        if sep and head:
            dirs.add(head)

    # A change to the workspace root manifest can change the very lint table the
    # opted-in crates inherit, so it puts all of them in scope.
    if ROOT_MANIFEST in changed:
        all_dirs = _crate_dirs_with_manifests(repo)
        if isinstance(all_dirs, Undetermined):
            return all_dirs
        dirs.update(all_dirs)

    selected: list[tuple[str, str]] = []
    for crate_dir in sorted(dirs):
        manifest_path = os.path.join(repo, "crates", crate_dir, "Cargo.toml")
        if not os.path.exists(manifest_path):
            # No manifest at all is a determinable answer, not a failure: this is
            # the skill-only plugin shape (crates/scout, crates/daily-report),
            # which is not a cargo crate and cannot opt in to anything.
            continue
        manifest = _read_manifest(manifest_path)
        if isinstance(manifest, Undetermined):
            return manifest
        if not _opts_in(manifest):
            print(
                f"  not opted in ([lints] workspace = true absent): "
                f"crates/{crate_dir}"
            )
            continue
        name = _package_name(manifest)
        if name is None:
            return Undetermined(
                f"{manifest_path} opts in to the workspace lints but has no "
                f"readable [package] name, so the unit to check is unknown"
            )
        selected.append((name, crate_dir))

    return Selection(tuple(selected))


def resolve_cargo() -> Undetermined | str:
    """The cargo executable, or Undetermined.

    Order: the explicit test seam, then PATH, then the rustup default location
    (CLAUDE.md: the toolchain is installed via rustup, and a hook can run with a
    PATH that has not sourced ~/.cargo/env). Anything that is present but not
    executable is Undetermined rather than skipped — "there is a cargo here that
    I cannot run" is not "there is nothing to run".
    """
    override = os.environ.get(CARGO_ENV)
    if override:
        if not os.path.isfile(override):
            return Undetermined(f"{CARGO_ENV}={override} is not a file")
        if not os.access(override, os.X_OK):
            return Undetermined(f"{CARGO_ENV}={override} is not executable")
        return override

    found = shutil.which("cargo")
    if found:
        return found

    fallback = os.path.join(os.path.expanduser("~"), ".cargo", "bin", "cargo")
    if os.path.isfile(fallback):
        if not os.access(fallback, os.X_OK):
            return Undetermined(f"{fallback} exists but is not executable")
        return fallback

    return Undetermined(
        "cargo not found on PATH or at ~/.cargo/bin/cargo, so the clippy check "
        "could not be run at all"
    )


def run_clippy(cargo: str, repo: str, package: str) -> Undetermined | bool:
    """True if clippy passed, False if it reported a violation.

    The verdict is `proc.returncode`, nothing else. A failure to launch is
    Undetermined, never a pass.
    """
    cmd = (cargo, "clippy", "-p", package, "--all-targets")
    print(f"  running: cargo clippy -p {package} --all-targets")
    try:
        proc = subprocess.run(
            cmd,
            cwd=repo,
            capture_output=True,
            text=True,
        )
    except (OSError, subprocess.SubprocessError) as exc:
        return Undetermined(f"could not run `{' '.join(cmd)}`: {exc}")

    if proc.returncode == 0:
        return True

    sys.stderr.write(f"\n--- cargo clippy -p {package} --all-targets (exit {proc.returncode}) ---\n")
    sys.stderr.write(proc.stdout)
    sys.stderr.write(proc.stderr)
    return False


UNDETERMINED_NOTE = """
clippy-lints: UNDETERMINED — {reason}.

This blocks (exit 2). "Could not check" is not "clean": an uninspected surface
carries no evidence either way, and recording it as a pass would make this gate
indistinguishable from one that never ran. Fix what stopped the scanner rather
than loosening it.
"""

VIOLATION_NOTE = """
clippy-lints: blocked — {names} violate the workspace deny lints
(clippy::unwrap_used / expect_used / panic), which they opted in to via
`[lints] workspace = true`.

Those three lints are how a verdict-bearing crate is stopped from turning
"cannot determine" into a crash. Fix the flagged expressions; do not remove the
opt-in to make this pass.
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

    selection = select_crates(repo)
    if isinstance(selection, Undetermined):
        sys.stderr.write(UNDETERMINED_NOTE.format(reason=selection.reason))
        return 2

    if not selection.crates:
        # Determined, and the answer is "none". Distinct from Undetermined above.
        print(
            "clippy-lints: no crate with `[lints] workspace = true` was changed "
            "in the working tree — nothing to check"
        )
        return 0

    cargo = resolve_cargo()
    if isinstance(cargo, Undetermined):
        sys.stderr.write(UNDETERMINED_NOTE.format(reason=cargo.reason))
        return 2

    print(
        "clippy-lints: checking "
        + ", ".join(f"{name} (crates/{d})" for name, d in selection.crates)
    )

    failed: list[str] = []
    for name, _crate_dir in selection.crates:
        verdict = run_clippy(cargo, repo, name)
        if isinstance(verdict, Undetermined):
            sys.stderr.write(UNDETERMINED_NOTE.format(reason=verdict.reason))
            return 2
        if not verdict:
            failed.append(name)

    if failed:
        sys.stderr.write(VIOLATION_NOTE.format(names=", ".join(failed)))
        return 1

    print("clippy-lints: clean")
    return 0


if __name__ == "__main__":
    sys.exit(main())
