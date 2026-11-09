#!/usr/bin/env python3
"""SessionStart hook: if the session is starting in the MAIN checkout, create a
session worktree and direct the agent into it.

CLAUDE.md 最上位の方針 8: no work happens on the main tree. This hook makes the
worktree exist at session start so the agent has somewhere to go — it checks the
starting checkout and, when it is the main tree (git-dir == git-common-dir),
creates `session-<id>` off HEAD in a repo-external directory and prints the
instruction to `cd` into it. (The Bash tool's cwd persists across calls, so a
single `cd` moves the whole session in.) The edit/commit guards do the actual
enforcing; this hook removes the excuse of "there was no worktree".

It also REGISTERS the worktree it creates (backlog 491f6e94, user ruling
2026-09-30): a `*.driver` record, in the shape `backlog driver` writes and
condukt's `wt_reconcile::registrations_in` reads, at
`$HOME/.backlog/drivers/session-worktrees/<fnv1a64(session_id)>.driver`.
`condukt session-heartbeat` (condukt's PostToolUse / UserPromptSubmit hook)
refreshes its `heartbeat_at` while the session works; once it ages out and the
branch is merged and the tree clean, `condukt worktree reap` may remove the
worktree. The bucket is NOT a backlog project slug on purpose — see
`SESSION_WORKTREE_BUCKET` in crates/condukt/src/wt_reap.rs.

It also lists the worktree in the workspace-trust list `~/.harness/trust.toml`
(user ruling 2026-11-08: trust is no longer inherited from the main working
tree, so the creator registers what it creates), exactly when the main tree is
itself listed — the rule condukt's `worktree::create` applies. A failed trust
registration is printed loudly; the gates then block that worktree as
config-untrusted (they do not fall back to defaults), and `<gate> trust` inside
the worktree is the remedy.

A worktree it creates is warm-seeded (backlog 9d9a429e): `seed-worktree-target.py`
clones the main tree's `target/` by APFS clonefile and syncs mtimes only for
tracked files whose content is identical, so the first cargo build is
incremental instead of cold. Cloning a large target takes minutes (92s measured
for 19G), so the seed is started DETACHED (backlog 675bc422, user ruling): new
session, stdin from /dev/null, stdout/stderr to a log file inside the
worktree's git dir; the hook does not wait. It prints `seed log: <path>`; the
seeder's verdict JSON (seeded / declined / failed / crashed) lands there. A
seed that cannot even be started writes its own `seeded: false` verdict to the
log. A declined or failed seed never prevents the worktree.

Fail-soft: this is a setup hook, not a gate. Any error prints a best-effort note
and exits 0 — the edit-time guards and the pre-commit check still block the main
tree regardless of whether this hook managed to pre-make the worktree. A failed
REGISTRATION is soft in the safe direction: an unregistered worktree is
undetermined to the reaper and is kept forever; the failure is printed.
"""

from __future__ import annotations

import json
import os
import subprocess
import sys
import time


def _git(cwd: str, *args: str) -> str | None:
    try:
        out = subprocess.run(
            ("git", *args), cwd=cwd, capture_output=True, text=True, timeout=15
        )
    except (OSError, subprocess.SubprocessError):
        return None
    return out.stdout.strip() if out.returncode == 0 else None


SESSION_WORKTREE_BUCKET = "session-worktrees"


def _fnv1a64(data: bytes) -> int:
    """harness_core::hash::fnv1a64 — the hash backlog names driver files by."""
    h = 0xCBF29CE484222325
    for b in data:
        h ^= b
        h = (h * 0x100000001B3) & 0xFFFFFFFFFFFFFFFF
    return h


def _harness_home() -> str | None:
    """Mirror of harness_core::config::home() (resolve_home): a declared
    `$HARNESS_SANDBOX_HOME` wins unless `$HOME` is set to a directory provably
    different from the account home; else `$HOME`. None = undetermined (the
    caller then does not write — no guessing the real home)."""
    home = os.environ.get("HOME") or None
    sandbox = os.environ.get("HARNESS_SANDBOX_HOME") or None
    if sandbox:
        try:
            import pwd

            acct = pwd.getpwuid(os.getuid()).pw_dir or None
        except (ImportError, KeyError, OSError):
            acct = None
        if home and acct and os.path.realpath(home) != os.path.realpath(acct):
            return home
        return sandbox
    return home


def _trust_path() -> str | None:
    home = _harness_home()
    return os.path.join(home, ".harness", "trust.toml") if home else None


def _read_trust_strict(path: str) -> list[str]:
    """harness_core::trust::read_file_strict: absent = empty; unreadable or
    unparseable raises (the caller must not write back a list that lost
    entries)."""
    import tomllib

    try:
        with open(path, "rb") as fh:
            data = tomllib.load(fh)
    except FileNotFoundError:
        return []
    trusted = data.get("trusted", [])
    if not isinstance(trusted, list) or not all(isinstance(t, str) for t in trusted):
        raise ValueError(f"trust list {path}: `trusted` is not a list of strings")
    return trusted


def _register_trust(root: str, wt_path: str) -> str | None:
    """Register `wt_path` in ~/.harness/trust.toml at creation — the same rule
    condukt's worktree::create applies (crates/condukt/src/worktree.rs, the
    `trust::is_listed(repo)` → `trust::add(path)` → read-back block).

    There is no trust inheritance (user ruling 2026-11-08): a linked worktree
    is trusted only when it is itself listed, so the creator lists it. Only a
    worktree cut from a repo that is itself LISTED is registered (membership,
    never `HARNESS_TRUST_ALL` — propagation is not a grant). Reads only the
    trust list; nothing from any other worktree.

    Same write discipline as harness_core::trust::add: exclusive flock on
    `trust.toml.lock` (bounded wait), strict read (an unparseable list is
    never rewritten), atomic tmp + fsync + rename, then a read-back.

    Returns None when registered or when the repo is not listed (nothing to
    propagate), else the reason registration failed.
    """
    import fcntl

    path = _trust_path()
    if path is None:
        return "home directory undetermined ($HOME unset)"
    root_key = os.path.realpath(root)
    wt_key = os.path.realpath(wt_path)
    lock_path = path + ".lock"
    try:
        os.makedirs(os.path.dirname(path), exist_ok=True)
        lock_fd = os.open(lock_path, os.O_CREAT | os.O_WRONLY, 0o644)
    except OSError as e:
        return f"cannot open trust lock {lock_path}: {e}"
    try:
        deadline = time.monotonic() + 10.0
        while True:
            try:
                fcntl.flock(lock_fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                break
            except BlockingIOError:
                if time.monotonic() >= deadline:
                    return f"timed out waiting for trust lock {lock_path}"
                time.sleep(0.005)
            except OSError as e:
                return f"cannot lock trust list {lock_path}: {e}"
        try:
            trusted = _read_trust_strict(path)
        except Exception as e:  # noqa: BLE001 — any read/parse failure refuses the write
            return f"trust list {path} unreadable; refusing to rewrite it: {e}"
        if root_key not in trusted:
            return None  # repo not listed: nothing to propagate (never escalate)
        if wt_key not in trusted:
            trusted.append(wt_key)
            body = "trusted = [" + ", ".join(json.dumps(t) for t in trusted) + "]\n"
            tmp = f"{path}.tmp.{os.getpid()}"
            try:
                with open(tmp, "w", encoding="utf-8") as fh:
                    fh.write(body)
                    fh.flush()
                    os.fsync(fh.fileno())
                os.replace(tmp, path)
                dfd = os.open(os.path.dirname(path), os.O_RDONLY)
                try:
                    os.fsync(dfd)
                finally:
                    os.close(dfd)
            except OSError as e:
                try:
                    os.remove(tmp)
                except OSError:
                    pass
                return f"cannot write trust list {path}: {e}"
        try:
            listed = wt_key in _read_trust_strict(path)
        except Exception as e:  # noqa: BLE001
            return f"trust list {path} unreadable on read-back: {e}"
        if not listed:
            return f"{wt_key} is not listed in {path} after the write"
        return None
    finally:
        os.close(lock_fd)


def _register_worktree(wt_path: str, session_id: str) -> str | None:
    """Write (or refresh) this session's registration for `wt_path`.

    Returns None on success, else the reason it could not be written.
    """
    home = os.environ.get("HOME")
    if not home:
        return "$HOME is unset"
    bucket = os.path.join(home, ".backlog", "drivers", SESSION_WORKTREE_BUCKET)
    path = os.path.join(bucket, f"{_fnv1a64(session_id.encode()):016x}.driver")
    now = int(time.time())
    registered_at = now
    try:
        with open(path, encoding="utf-8") as fh:
            prev = json.load(fh)
        if isinstance(prev.get("registered_at"), int):
            registered_at = prev["registered_at"]
    except (OSError, ValueError, AttributeError):
        pass
    record = {
        "session_id": session_id,
        "pid": os.getpid(),
        "project": os.path.realpath(wt_path),
        "registered_at": registered_at,
        "heartbeat_at": now,
    }
    tmp = f"{path}.tmp.{os.getpid()}"
    try:
        os.makedirs(bucket, exist_ok=True)
        with open(tmp, "w", encoding="utf-8") as fh:
            json.dump(record, fh, indent=2)
        os.replace(tmp, path)
    except OSError as e:
        try:
            os.remove(tmp)
        except OSError:
            pass
        return str(e)
    return None


def _report_rollout_drift(root: str) -> None:
    """Print the rollout-drift notice, if there is one to print.

    Fail-soft by the same rule as the rest of this hook: a setup hook must not
    break session start. Note the asymmetry — the DRIFT check itself is not
    fail-soft about its own verdict (an unverifiable check prints UNDETERMINED
    rather than nothing, see session-rollout-drift.py). What is soft here is
    only this hook's ability to reach it at all.
    """
    helper = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                          "session-rollout-drift.py")
    if not os.path.isfile(helper):
        return
    try:
        out = subprocess.run(
            (sys.executable or "python3", helper, root),
            capture_output=True, text=True, timeout=90,
        )
    except (OSError, subprocess.SubprocessError):
        return
    text = (out.stdout or "").strip()
    if text:
        print(text)


def _seed_log_path(wt_path: str) -> str:
    """Absolute log path inside the worktree's own git dir (removed with the
    worktree, never an untracked file in it). Falls back to the temp dir."""
    gd = _git(wt_path, "rev-parse", "--absolute-git-dir")
    if gd and os.path.isdir(gd):
        return os.path.join(os.path.realpath(gd), "seed-target.log")
    import tempfile
    return os.path.join(os.path.realpath(tempfile.gettempdir()),
                        f"seed-target-{os.path.basename(wt_path)}-{os.getpid()}.log")


def _seed_target(warm_root: str, wt_path: str) -> str:
    """Start the warm seed of the new worktree's target/ DETACHED (backlog
    9d9a429e / 675bc422) and return one short line naming the seed log.

    The child gets a new session, stdin=/dev/null and stdout/stderr on the log
    file only, so it holds none of this hook's pipes and the hook returns at
    once. The seeder writes its verdict JSON (including declines and crashes)
    to that log. If the seeder cannot even be started, this writes a
    `seeded: false` verdict to the log itself. Never raises: a failed seed only
    means the first cargo build is cold — the worktree already exists.
    """
    seeder = os.path.join(os.path.dirname(os.path.abspath(__file__)),
                          "seed-worktree-target.py")
    log = _seed_log_path(wt_path)
    try:
        fh = open(log, "a", encoding="utf-8")
    except OSError as e:
        return (f"[worktree-init] target/ not seeded (cannot open seed log {log}: "
                f"{e}); first cargo build is cold")
    with fh:
        fh.write(f"[worktree-init] seeding {wt_path} from {warm_root}\n")
        fh.flush()
        try:
            child = subprocess.Popen(
                (sys.executable or "python3", seeder, warm_root, wt_path),
                stdin=subprocess.DEVNULL, stdout=fh, stderr=fh,
                start_new_session=True, close_fds=True,
            )
        except (OSError, subprocess.SubprocessError) as e:
            fh.write(json.dumps({"seeded": False,
                                 "reason": f"seeder could not be started: {e}"}) + "\n")
            return (f"[worktree-init] target/ not seeded (seeder could not start: {e}); "
                    f"seed log: {log}")
    return (f"[worktree-init] target/ warm seed running in background (pid {child.pid}); "
            f"seed log: {log}")


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        payload = {}
    cwd = (payload.get("cwd") if isinstance(payload, dict) else None) or os.getcwd()
    sid = (payload.get("session_id") if isinstance(payload, dict) else None) or "adhoc"
    short = str(sid).split("-")[0][:8]

    git_dir = _git(cwd, "rev-parse", "--absolute-git-dir")
    common = _git(cwd, "rev-parse", "--path-format=absolute", "--git-common-dir")
    if git_dir is None or common is None:
        return 0  # not a git repo — nothing to do

    # Rollout drift, reported where an operator can act (backlog b6a80342).
    # Called from here rather than registered as its own SessionStart hook
    # because .claude/settings.json is deny-listed for editing; this hook is
    # already wired, so it is the reachable surface. Printed for worktree
    # sessions too — drift is a property of the machine, not of the checkout.
    _report_rollout_drift(_git(cwd, "rev-parse", "--show-toplevel") or cwd)

    if os.path.realpath(git_dir) != os.path.realpath(common):
        # Already in a worktree — the session is where it should be.
        print("[worktree-init] session is in a worktree ✓")
        return 0

    root = _git(cwd, "rev-parse", "--show-toplevel") or cwd
    base = os.path.basename(root.rstrip("/"))
    wt_parent = os.path.join(os.path.dirname(root.rstrip("/")), f".{base}-worktrees")
    branch = f"session-{short}"
    wt_path = os.path.join(wt_parent, branch)

    try:
        os.makedirs(wt_parent, exist_ok=True)
    except OSError:
        pass

    created = False
    if not os.path.isdir(wt_path):
        # Fresh worktree off current HEAD. If the branch name is taken, fall back
        # to attaching without -b so we still land in a worktree.
        made = subprocess.run(
            ("git", "worktree", "add", "-b", branch, wt_path, "HEAD"),
            cwd=root, capture_output=True, text=True,
        )
        if made.returncode != 0:
            subprocess.run(
                ("git", "worktree", "add", wt_path, "HEAD"),
                cwd=root, capture_output=True, text=True,
            )
        created = os.path.isdir(wt_path)

    if os.path.isdir(wt_path):
        # Trust is per root (no inheritance, user ruling 2026-11-08). Done for a
        # reused worktree too, so one created before this rule gets listed.
        terr = _register_trust(root, wt_path)
        if terr is not None:
            print(
                f"⚠ [worktree-init] could NOT register {wt_path} in the workspace "
                f"trust list ({terr}). Gates that find a project config there "
                "(donegate / reviewgate / propguard / tdd / precommit-audit) will "
                "BLOCK as config-untrusted until you run `donegate trust` (or the "
                "gate's own `trust`) inside the worktree."
            )
        err = _register_worktree(wt_path, str(sid))
        if err is not None:
            print(
                f"[worktree-init] could not register {wt_path} ({err}); it stays "
                "unregistered, so `condukt worktree reap` will never remove it."
            )
        print(
            "⚠ You started on the MAIN working tree. Direct edits/commits to it "
            "are BLOCKED (CLAUDE.md 8).\n"
            f"A session worktree is ready at:\n    {wt_path}\n"
            f"Run this before any edit — the Bash cwd persists across calls:\n"
            f"    cd {wt_path}\n"
            "Author everything there, commit in the worktree, then merge onto main."
        )
        if created:
            # Detached: the hook does not wait for the clone (92s for 19G).
            print(_seed_target(root, wt_path))
    else:
        print(
            "⚠ You are on the MAIN working tree and a session worktree could not "
            "be auto-created. Create one yourself before editing:\n"
            f"    git worktree add -b {branch} <path-outside-repo> HEAD && cd <path>"
        )
    return 0


if __name__ == "__main__":
    sys.exit(main())
