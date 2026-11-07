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
  * if an mtime write fails midway, the already-touched mtimes are restored and
    the target is removed. If THAT rollback fails the state is not clean: the
    script says so and exits 1 (it does not claim a clean decline).

Output: exit 0 when it seeded or cleanly declined (a declined seed is not an
error; the caller just cold-builds). The LAST stdout line is one JSON object
{"seeded": true|false, "reason": "...", ...}.
"""

from __future__ import annotations

import ctypes
import json
import os
import shutil
import stat
import subprocess
import sys

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


def _same_content(a: str, b: str) -> bool:
    with open(a, "rb") as fa, open(b, "rb") as fb:
        while True:
            ca = fa.read(1 << 20)
            cb = fb.read(1 << 20)
            if ca != cb:
                return False
            if not ca:
                return True


def _plan(warm: str, new: str) -> list[tuple[str, int, int]]:
    """Return [(new_path, warm_mtime_ns, warm_atime_ns)] for identical files.

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
            same = _same_content(wp, np_)
        except OSError as e:
            raise Decline(f"cannot compare {rel}: {e}")
        if same:
            plan.append((np_, ws.st_mtime_ns, ws.st_atime_ns))
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

    touched: list[tuple[str, int, int]] = []
    try:
        for path, mtime, _ in plan:
            st = os.lstat(path)
            touched.append((path, st.st_atime_ns, st.st_mtime_ns))
            os.utime(path, ns=(st.st_atime_ns, mtime), follow_symlinks=False)
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

    _emit(True, "cloned warm target and synced mtimes of identical tracked files",
          warm=warm, synced=len(plan))
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
