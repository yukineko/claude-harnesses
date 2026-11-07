#!/usr/bin/env python3
"""Warm-seed a fresh worktree's `target/` by APFS clonefile (backlog 9d9a429e).

Usage: seed-worktree-target.py WARM_ROOT NEW_ROOT

Clones WARM_ROOT/target into NEW_ROOT/target (`cp -c -R -p`, i.e. clonefile
with timestamps preserved) and then gives every tracked file whose working-tree
CONTENT is byte-identical in both trees the warm file's mtime, so cargo's
mtime fingerprints treat the clone as fresh for unchanged sources and rebuild
only what actually differs.

SAFETY: a file whose content differs (or cannot be shown to be equal) keeps its
own fresh mtime, so cargo rebuilds it. Syncing a changed file's mtime would make
cargo run tests against stale code — that is the fail-open this script must
never produce. Hence every doubt resolves to "do not seed":

  * no warm target, NEW_ROOT/target already present, WARM_ROOT not the top of a
    git checkout, `git ls-files` failing in either tree, a file that cannot be
    read for comparison, `cp` exiting non-zero, the final rename failing —
    all decline: NEW_ROOT/target is not created (an existing one is untouched)
    and no mtime under NEW_ROOT is changed.
  * the list of files to touch is fully decided (all comparisons done) before
    anything is cloned; mtimes are touched only after the clone is in place.
  * the clone goes to a temp dir inside NEW_ROOT and is renamed into place with
    renamex_np(RENAME_EXCL), so a failed clone never leaves a partial target and
    a target that appeared meanwhile is never replaced.
  * the mtime pass runs while holding LOCK_EX on every cargo lock file
    (`.cargo-lock`, `.cargo-build-lock`, `.cargo-artifact-lock`) of every
    profile dir in the clone, so no cargo build reads half-applied mtimes. If
    those locks cannot be taken (bounded retry, 30s or
    SEED_WORKTREE_TARGET_LOCK_TIMEOUT_SECS), the pass is skipped and the
    verdict is `seeded: false, target_left: true` — the clone is kept because
    cargo may be using it, and its mtimes stay fresh, which only costs a
    rebuild (backlog 675bc422 ruling).
  * EDIT RACE (backlog 675bc422): the seed runs in the background, so a file
    may be edited after it was judged identical. After each warm mtime is
    applied the file is re-read and re-compared with the warm content; if it
    differs or cannot be read its mtime is set to now, so cargo rebuilds it.
  * if an mtime write fails midway, the already-touched mtimes are restored and
    the target is removed. If THAT rollback fails the state is not clean: the
    script says so and exits 1 (it does not claim a clean decline). A crash
    still prints a `seeded: false` verdict (exit 1).

Test seam: SEED_WORKTREE_TARGET_PAUSE_DIR=<dir> makes the seeder, at the last
point before applying any warm mtime (plan decided, clone in place, locks
held), create <dir>/paused and wait for <dir>/resume (bounded by
SEED_WORKTREE_TARGET_PAUSE_TIMEOUT, default 300s; timeout declines: target
removed, no mtime touched).

Output: exit 0 when it seeded or cleanly declined (a declined seed is not an
error; the caller just cold-builds). The LAST stdout line is one JSON object
{"seeded": true|false, "reason": "...", ...}.
"""

from __future__ import annotations

import ctypes
import fcntl
import hashlib
import json
import os
import shutil
import stat
import subprocess
import sys
import time

RENAME_EXCL = 0x00000004  # <stdio.h> on darwin


class Decline(Exception):
    pass


def _emit(seeded: bool, reason: str, **extra) -> None:
    print(json.dumps({"seeded": seeded, "reason": reason, **extra}))


def _git_ls_files(root: str) -> list[str]:
    try:
        r = subprocess.run(
            ("git", "-C", root, "ls-files", "-z"),
            capture_output=True, timeout=120,
        )
    except (OSError, subprocess.SubprocessError) as e:
        raise Decline(f"git ls-files could not run in {root}: {e}")
    if r.returncode != 0:
        raise Decline(f"git ls-files exited {r.returncode} in {root}: "
                      f"{r.stderr.decode(errors='replace').strip()}")
    return [p for p in r.stdout.decode("utf-8", "surrogateescape").split("\0") if p]


def _toplevel(root: str) -> str:
    try:
        r = subprocess.run(
            ("git", "-C", root, "rev-parse", "--show-toplevel"),
            capture_output=True, text=True, timeout=30,
        )
    except (OSError, subprocess.SubprocessError) as e:
        raise Decline(f"git rev-parse could not run in {root}: {e}")
    if r.returncode != 0:
        raise Decline(f"{root} is not a git checkout: {r.stderr.strip()}")
    return os.path.realpath(r.stdout.strip())


def _same_content(a: str, b: str) -> bytes | None:
    """sha256 of `a` if `a` and `b` are byte-identical, else None."""
    h = hashlib.sha256()
    with open(a, "rb") as fa, open(b, "rb") as fb:
        while True:
            ca = fa.read(1 << 20)
            cb = fb.read(1 << 20)
            if ca != cb:
                return None
            if not ca:
                return h.digest()
            h.update(ca)


def _digest(path: str) -> bytes:
    h = hashlib.sha256()
    with open(path, "rb") as fh:
        while True:
            c = fh.read(1 << 20)
            if not c:
                return h.digest()
            h.update(c)


def _plan(warm: str, new: str) -> list[tuple[str, int, bytes]]:
    """Return [(new_path, warm_mtime_ns, warm_sha256)] for identical files.

    Raises Decline whenever a comparison cannot be made.
    """
    warm_tracked = set(_git_ls_files(warm))
    plan = []
    for rel in _git_ls_files(new):
        if rel not in warm_tracked:
            continue  # determined difference: not tracked on the warm side
        np_, wp = os.path.join(new, rel), os.path.join(warm, rel)
        try:
            ns = os.lstat(np_)
        except FileNotFoundError:
            continue  # deleted in the new tree: nothing to touch
        except OSError as e:
            raise Decline(f"cannot stat {np_}: {e}")
        if not stat.S_ISREG(ns.st_mode):
            continue  # symlink / gitlink: never synced (stays fresh)
        try:
            ws = os.lstat(wp)
        except FileNotFoundError:
            continue  # determined difference
        except OSError as e:
            raise Decline(f"cannot stat {wp}: {e}")
        if not stat.S_ISREG(ws.st_mode) or ws.st_size != ns.st_size:
            continue  # determined difference
        try:
            digest = _same_content(wp, np_)
        except OSError as e:
            raise Decline(f"cannot compare {rel}: {e}")
        if digest is not None:
            plan.append((np_, ws.st_mtime_ns, digest))
    return plan


def _rename_excl(src: str, dst: str) -> None:
    try:
        libc = ctypes.CDLL(None, use_errno=True)
        fn = libc.renamex_np
    except (OSError, AttributeError) as e:
        raise Decline(f"renamex_np unavailable (not macOS?): {e}")
    fn.argtypes = (ctypes.c_char_p, ctypes.c_char_p, ctypes.c_uint)
    fn.restype = ctypes.c_int
    if fn(os.fsencode(src), os.fsencode(dst), RENAME_EXCL) != 0:
        err = ctypes.get_errno()
        raise Decline(f"rename into {dst} failed: {os.strerror(err)}")


def _clone(warm_target: str, new: str, new_target: str) -> None:
    tmp = os.path.join(new, f".target.seed-{os.getpid()}.tmp")
    if os.path.lexists(tmp):
        raise Decline(f"temp path {tmp} already exists")
    try:
        try:
            r = subprocess.run(
                ("cp", "-c", "-R", "-p", warm_target, tmp),
                capture_output=True, text=True,
            )
        except (OSError, subprocess.SubprocessError) as e:
            raise Decline(f"cp could not run: {e}")
        if r.returncode != 0:
            tail = r.stderr.strip().splitlines()[-1:] or [""]
            raise Decline(f"cp -c exited {r.returncode}: {tail[0]}")
        _rename_excl(tmp, new_target)
    finally:
        if os.path.lexists(tmp):
            shutil.rmtree(tmp, ignore_errors=True)
            if os.path.lexists(tmp):
                print(f"[seed] warning: could not remove temp clone {tmp}",
                      file=sys.stderr)


LOCK_NAMES = (".cargo-lock", ".cargo-build-lock", ".cargo-artifact-lock")
LOCK_WAIT_S = 30.0
_NOT_PROFILE = {"deps", "build", "incremental", "examples", ".fingerprint"}


def _lock_files(target: str) -> list[str]:
    """Every cargo lock file in every profile dir of `target` (depth <= 2, so
    `target/<profile>` and `target/<triple-or-custom>/<profile>`).

    Probed against cargo 1.97.1: a build blocks while any of `.cargo-lock`,
    `.cargo-build-lock`, `.cargo-artifact-lock` in its profile dir is held
    LOCK_EX, so holding all three keeps every cargo build out of the clone.
    """
    found = []
    for dirpath, dirnames, filenames in os.walk(target):
        rel = os.path.relpath(dirpath, target)
        depth = 0 if rel == "." else rel.count(os.sep) + 1
        if depth >= 2:
            dirnames[:] = []
        else:
            dirnames[:] = [d for d in dirnames if d not in _NOT_PROFILE]
        found += [os.path.join(dirpath, n) for n in filenames if n in LOCK_NAMES]
    return sorted(found)


def _take_cargo_locks(target: str) -> list[int] | None:
    """LOCK_EX|LOCK_NB on every lock file, all-or-nothing, retried for a bounded
    time. Non-blocking so it cannot deadlock with a cargo that holds one lock
    and waits on another. None = could not take them all, or none exist (a
    clone with no lock files cannot be shown to be cargo-free)."""
    paths = _lock_files(target)
    if not paths:
        return None
    try:
        wait = float(os.environ.get("SEED_WORKTREE_TARGET_LOCK_TIMEOUT_SECS",
                                    LOCK_WAIT_S))
    except ValueError:
        wait = LOCK_WAIT_S
    # Any bound is safe here: giving up only skips the mtime pass.
    end = time.monotonic() + max(0.0, wait)
    while True:
        fds: list[int] = []
        try:
            for p in paths:
                fd = os.open(p, os.O_RDWR)
                fds.append(fd)
                fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
            return fds
        except OSError:
            for fd in fds:
                os.close(fd)
            if time.monotonic() >= end:
                return None
            time.sleep(0.2)


def _pause_seam() -> bool:
    """Test seam SEED_WORKTREE_TARGET_PAUSE_DIR, on the real path: the last
    point before any warm mtime is applied (plan decided, clone in place,
    cargo locks held). True = continue; False = bounded wait timed out."""
    d = os.environ.get("SEED_WORKTREE_TARGET_PAUSE_DIR")
    if not d:
        return True
    try:
        timeout = float(os.environ.get("SEED_WORKTREE_TARGET_PAUSE_TIMEOUT", "300"))
    except ValueError:
        timeout = 300.0
    with open(os.path.join(d, "paused"), "w"):
        pass
    end = time.monotonic() + timeout
    while not os.path.exists(os.path.join(d, "resume")):
        if time.monotonic() >= end:
            return False
        time.sleep(0.05)
    return True


def _apply(plan, touched) -> int:
    """Apply warm mtimes, re-verifying each file AFTER its utime. A file that no
    longer matches the warm content (edited since planning, including between
    the check and the utime) or cannot be read gets mtime=now, so cargo
    rebuilds it. Returns how many were reset to now."""
    reset = 0
    for path, mtime, digest in plan:
        st = os.lstat(path)
        touched.append((path, st.st_atime_ns, st.st_mtime_ns))
        os.utime(path, ns=(st.st_atime_ns, mtime), follow_symlinks=False)
        try:
            ok = _digest(path) == digest
        except OSError:
            ok = False
        if not ok:
            now = time.time_ns()
            os.utime(path, ns=(now, now), follow_symlinks=False)
            touched.pop()  # a now-mtime is never stale; nothing to roll back
            reset += 1
    return reset


def main(argv: list[str]) -> int:
    if len(argv) != 3:
        _emit(False, "usage: seed-worktree-target.py WARM_ROOT NEW_ROOT")
        return 0
    warm, new = os.path.realpath(argv[1]), os.path.realpath(argv[2])
    warm_target = os.path.join(warm, "target")
    new_target = os.path.join(new, "target")
    try:
        if os.path.islink(warm_target) or not os.path.isdir(warm_target):
            raise Decline(f"no warm target at {warm_target}")
        if os.path.lexists(new_target):
            raise Decline(f"{new_target} already exists; left untouched")
        if _toplevel(warm) != warm:
            raise Decline(f"{warm} is not the top of a git checkout")
        if _toplevel(new) != new:
            raise Decline(f"{new} is not the top of a git checkout")
        plan = _plan(warm, new)
        _clone(warm_target, new, new_target)
    except Decline as d:
        _emit(False, str(d))
        return 0

    fds = _take_cargo_locks(new_target)
    if fds is None:
        # Cargo may already be using the clone: do not remove it. Mtimes stay
        # fresh, so cargo rebuilds the workspace crates (cost, never staleness).
        _emit(False, "target cloned but cargo's build-dir locks could not be "
              "taken; mtime sync skipped (workspace crates will rebuild)",
              target_left=True)
        return 0
    touched: list[tuple[str, int, int]] = []
    try:
        if not _pause_seam():
            shutil.rmtree(new_target, ignore_errors=True)
            if os.path.lexists(new_target):
                _emit(False, "pause seam timed out and target removal failed",
                      target_left=True)
                return 1
            _emit(False, "pause seam timed out; target removed, no mtime touched")
            return 0
        try:
            reset = _apply(plan, touched)
        except OSError as e:
            failed = []
            for path, at, mt in touched:
                try:
                    os.utime(path, ns=(at, mt), follow_symlinks=False)
                except OSError:
                    failed.append(path)
            shutil.rmtree(new_target, ignore_errors=True)
            if failed or os.path.lexists(new_target):
                _emit(False, f"mtime sync failed ({e}) and rollback was incomplete",
                      rollback_failed=failed, target_left=os.path.lexists(new_target))
                return 1
            _emit(False, f"mtime sync failed ({e}); rolled back")
            return 0
    finally:
        for fd in fds:
            os.close(fd)

    _emit(True, "cloned warm target and synced mtimes of identical tracked files",
          warm=warm, synced=len(plan) - reset, changed_during_seed=reset)
    return 0


def _main_guarded(argv: list[str]) -> int:
    """A crash must still leave a verdict line (the background log reads it).
    Every warm mtime that was applied was re-verified per file, so a crash can
    leave a target and some synced mtimes, but no stale-looking-fresh file."""
    try:
        return main(argv)
    except BaseException as e:  # reported, then non-zero
        _emit(False, f"seeder crashed: {type(e).__name__}: {e}",
              state="undetermined")
        return 1


if __name__ == "__main__":
    sys.exit(_main_guarded(sys.argv))
