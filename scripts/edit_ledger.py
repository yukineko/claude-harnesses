#!/usr/bin/env python3
"""Per-session ledger of MAIN-tree writes that the maintree guards ALLOWED
(backlog 233f819c; CLAUDE.md section 8 attribution).

Why: stop-verify-worktree.py must block a session that ends on a dirty main
tree, but another concurrent session may legitimately own some of that dirt
(e.g. conflict resolution during a merge). Without attribution the Stop gate
either blocks the wrong session or waves everything through. This ledger is the
positive evidence of who wrote what.

Location: $HARNESS_EDIT_LEDGER_DIR if set, else ~/.claude/state/maintree-edits.
One file per session: <dir>/<session_id>.jsonl, JSON Lines:
    {"session_id": "<sid>", "root": "<realpath of main toplevel>",
     "path": "<root-relative posix path>", "ts": <float epoch>}
A line that is not such an object, or whose session_id differs from the file
name, makes the ledger CORRUPT.

API:
  record(session_id, repo_root, paths) -> None   raises on any IO failure or an
      unusable session_id. Guards call it ONLY for operations they ALLOW on
      main; a refused edit wrote nothing and is not recorded.
  owners(repo_root) -> Known(dict[path -> set[session_id]]) | Undetermined
      Reads every session file. Undetermined (never an empty dict) when the
      directory or any file is unreadable or corrupt. An absent or empty
      directory is Known({}). Only entries whose root equals repo_root count.

Undeterminable attribution is never "not owned": consumers must block.
"""

from __future__ import annotations

import json
import os
import re
import time

_SESSION_RE = re.compile(r"^[A-Za-z0-9._-]{1,128}$")
LEDGER_SUBDIR = os.path.join(".claude", "state", "maintree-edits")


class Known:
    def __init__(self, value):
        self.value = value


class Undetermined:
    def __init__(self, why: str):
        self.why = why


def _valid_sid(sid) -> bool:
    return isinstance(sid, str) and bool(_SESSION_RE.match(sid)) and sid not in (".", "..")


def ledger_dir() -> str:
    env = os.environ.get("HARNESS_EDIT_LEDGER_DIR")
    if env:
        return env
    home = os.path.expanduser("~")
    if not home or not os.path.isabs(home):
        raise OSError("the home directory is not an absolute path")
    return os.path.join(home, LEDGER_SUBDIR)


def _rel(root: str, path: str) -> str:
    root_r = os.path.realpath(root)
    if os.path.isabs(path):
        # realpath the parent only so a not-yet-existing leaf still resolves.
        path = os.path.join(os.path.realpath(os.path.dirname(path)), os.path.basename(path))
    else:
        path = os.path.join(root_r, path)
    rel = os.path.relpath(path, root_r)
    if rel == ".." or rel.startswith(".." + os.sep):
        raise ValueError(f"{path!r} is not under {root_r!r}")
    return rel.replace(os.sep, "/")


def record(session_id: str, repo_root: str, paths: list[str]) -> None:
    if not _valid_sid(session_id):
        raise ValueError(f"unusable session_id {session_id!r}")
    root = os.path.realpath(repo_root)
    now = time.time()
    lines = [json.dumps({"session_id": session_id, "root": root,
                         "path": _rel(root, p), "ts": now}) + "\n" for p in paths]
    d = ledger_dir()
    os.makedirs(d, exist_ok=True)
    fd = os.open(os.path.join(d, session_id + ".jsonl"),
                 os.O_WRONLY | os.O_APPEND | os.O_CREAT | getattr(os, "O_NOFOLLOW", 0), 0o600)
    try:
        os.write(fd, "".join(lines).encode())
    finally:
        os.close(fd)


def owners(repo_root: str):
    try:
        root = os.path.realpath(repo_root)
        d = ledger_dir()
        try:
            names = sorted(os.listdir(d))
        except FileNotFoundError:
            return Known({})
        out: dict[str, set[str]] = {}
        for name in names:
            if not name.endswith(".jsonl"):
                continue
            sid = name[: -len(".jsonl")]
            with open(os.path.join(d, name), "rb") as fh:
                data = fh.read().decode("utf-8")
            for ln in data.splitlines():
                if not ln.strip():
                    continue
                obj = json.loads(ln)
                if (not isinstance(obj, dict)
                        or not all(isinstance(obj.get(k), str)
                                   for k in ("session_id", "root", "path"))
                        or not isinstance(obj.get("ts"), (int, float))
                        or obj["session_id"] != sid):
                    return Undetermined(f"corrupt entry in {name}")
                if os.path.realpath(obj["root"]) == root:
                    out.setdefault(obj["path"], set()).add(sid)
        return Known(out)
    except Exception as e:  # noqa: BLE001 — unreadable/corrupt is undetermined
        return Undetermined(f"{type(e).__name__}: {e}")
