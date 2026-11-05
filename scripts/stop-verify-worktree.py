#!/usr/bin/env python3
"""Stop hook: refuse to stop while sitting on the MAIN checkout with uncommitted
changes.

CLAUDE.md 最上位の方針 8 + the Stop-gate invariant (判定不能は block へ): the
session's work belongs in a worktree, so ending a turn with dirty, uncommitted
changes on the MAIN tree is the failure this catches. If the session is in a
worktree, or the main tree is clean, the stop is allowed. If it is on the main
tree AND dirty, the stop is BLOCKED so the changes get moved into a worktree
before the turn ends.

Fail-closed throughout (CLAUDE.md 3, 判定不能は制限側へ). Exactly ONE git outcome
allows the stop without an inspection: git itself reporting that this is not a
git repository, which means there is no main tree here to be dirty. Every other
failure of every probe — git not runnable, a timeout, a non-zero exit for any
other reason (dubious ownership, a corrupt repo, an unsupported rev-parse flag) —
leaves this hook unable to tell a worktree from a dirty main tree, and resolves
to a block. A probe that could not run has not passed.

That distinction is the whole point: the earlier version collapsed "git said no
repo" and "git could not answer" into one `return 0`, so any git malfunction
silently waved through the exact case this gate exists to catch.

Refused-then-changed (e033c406, signal 2 of scripts/deny_ledger.py). BEFORE the
cwd checks above — so it applies whether the session sits on main or in a
worktree — every target that guard-maintree-edit.py refused in this session
(guard-maintree-bash.py records nothing in the ledger since the 2026-10-04 ruling) (the per-session ledger
`~/.claude/state/maintree-deny/<session_id>.jsonl` — or, when that cannot be
written, the temp-dir fallback `<tmp>/maintree-deny-<uid>/<session_id>.jsonl`
(both are always read)) is re-snapshotted. If one
that is still under main has changed since its refusal (its `git status
--porcelain` line differs, or it is dirty now and its existence / type / mtime /
size differ), the stop is BLOCKED, quoting the refusal: the refused change was
made anyway by another spelling. An unreadable or corrupt ledger, a target whose
state cannot be read, or an unusable session_id also blocks — a corrupt (but
readable) ledger file only for 20 minutes from its mtime, after which it is
renamed aside to `<name>.corrupt-<ts>` (or `-<n>` suffixed when that name
exists; an aside is never replaced), its valid deny lines are carried into
a fresh ledger file (and still judged here), the unparseable lines are dropped
(named in a stderr notice) and the stop proceeds. An UNREADABLE ledger file
(e.g. mode 000) has no such bound and is never moved aside: nothing can be
salvaged from it, so it blocks until it is repaired, removed or cleared by the
next user prompt (issue #1182). A deny whose recorded main root no longer
exists cannot be re-snapshotted and blocks as undetermined until the ledger is
cleared or pruned. A payload without a session_id key has no ledger
(deny_ledger.py, RESIDUALS).

Bounded allow (per-gate, per-session marker — the design of harness_core's
`gate::run::run_guarded`). `stop_hook_active` alone says only that SOME Stop
hook blocked the previous stop; it does not say THIS gate did. So:

  * every time this gate blocks (exit 2, ledger or cwd check) it writes the
    marker `<state dir>/<session_id>`; every run that does not block removes it.
    The state dir is `$STOP_VERIFY_WORKTREE_STATE_DIR`, defaulting to
    `~/.claude/state/stop-verify-worktree/`.
  * first stop of a chain (`stop_hook_active` false): the full check runs and a
    violation blocks, whatever the marker says.
  * re-entry with this gate's marker present: this gate blocked the previous
    stop of the chain, so the stop is let through ONCE (the marker is consumed)
    with a `{"systemMessage": ...}` on stdout naming the bounded allow. The
    violation is NOT re-checked; the message says unverified, never clean.
  * re-entry with no marker: another gate blocked the previous stop. The full
    check runs and a violation still blocks (and writes the marker, so the next
    re-entry is bounded).
  * re-entry with no usable session_id, or a marker that cannot be read /
    removed / whose directory cannot be used: whether this gate blocked last
    cannot be told, and blocking would risk an unbounded trap, so the stop is an
    EXPLICIT allow carrying a `systemMessage` saying the check was not run. It
    is never silent.

A failure to write the marker when blocking the FIRST stop of a chain does not
soften that block (the re-entry that follows handles it). On a re-entry, a
block that cannot be recorded would make every later re-entry see "no marker"
and block forever, so it resolves to the same explicit allow + systemMessage
(as run_guarded's `decide` does).

    exit 0   allow the stop
    exit 2   block the stop; stderr is the reason shown to the model
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

try:
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import deny_ledger  # noqa: E402
    _LEDGER_IMPORT_ERROR = ""
except Exception as _e:  # noqa: BLE001 — resolves to a block in main()
    deny_ledger = None
    _LEDGER_IMPORT_ERROR = f"{type(_e).__name__}: {_e}"


def _git(cwd: str, *args: str):
    try:
        return subprocess.run(
            ("git", *args), cwd=cwd, capture_output=True, text=True, timeout=15
        )
    except (OSError, subprocess.SubprocessError):
        return None


# Probe outcomes. Three answers, not two — the same tri-state the rest of this
# repo's gates use (crates/blastguard/src/model.rs, harness_core::verdict).
OK = "ok"
NO_REPO = "no-repo"
UNDET = "undetermined"

# git's own wording for "there is no repository here", which is the ONLY
# non-zero exit that legitimately means "nothing to verify".
_NO_REPO_MARKERS = (
    "not a git repository",
    "not a git repo",  # older/localised phrasings that still name the class
)


def _probe(cwd: str, *args: str) -> tuple[str, str | None]:
    """Run a git query and classify the outcome as OK / NO_REPO / UNDET."""
    r = _git(cwd, *args)
    if r is None:
        return UNDET, None  # git not runnable, or it timed out
    if r.returncode == 0:
        value = r.stdout.strip()
        # A zero exit with no answer is still no answer.
        return (OK, value) if value else (UNDET, None)
    stderr = (r.stderr or "").lower()
    if any(marker in stderr for marker in _NO_REPO_MARKERS):
        return NO_REPO, None
    return UNDET, None


BLOCK = """Do not stop yet: this turn is ending on the MAIN working tree with uncommitted changes.

CLAUDE.md 最上位の方針 8: work is authored in a worktree, never left dirty on
main. Move the changes into a worktree and commit them there:

    git worktree add -b <branch> <path-outside-repo> HEAD
    git -C <path> ...    # or: stash, then apply inside the worktree
    # commit in the worktree, then merge onto main

Then it is safe to stop.
"""

UNDETERMINED = """Do not stop yet: could not determine whether this turn is ending dirty on main.

Failed probe: {what}

A check that could not run has not passed (CLAUDE.md 3), so this resolves to a
block rather than an allow. Note this is NOT the same as "there is no git
repository here" — git said something else, or could not be run at all. Re-run
from a valid checkout, or fix what is stopping git from answering.
"""


_MARKER_ENV = "STOP_VERIFY_WORKTREE_STATE_DIR"

# Marker read outcomes (UNDET is shared with the git probes above).
PRESENT = "present"
ABSENT = "absent"

_UNVERIFIED = ("so the stop is allowed WITHOUT a completed worktree / "
               "deny-ledger check (unverified, not clean).")


def _state_dir() -> str:
    d = os.environ.get(_MARKER_ENV)
    if d:
        return d
    return os.path.join(os.path.expanduser("~"), ".claude", "state",
                        "stop-verify-worktree")


def _session_id(payload) -> str | None:
    """The session id if it is usable as a single file name, else None."""
    if not isinstance(payload, dict):
        return None
    sid = payload.get("session_id")
    if not isinstance(sid, str) or sid in ("", ".", ".."):
        return None
    if any(c in sid for c in ("/", os.sep, chr(0))):
        return None
    return sid


def _marker_path(sid: str) -> str:
    return os.path.join(_state_dir(), sid)


def _take_marker(sid: str) -> tuple[str, str]:
    """Read and consume this gate's marker: (PRESENT | ABSENT | UNDET, detail)."""
    path = _marker_path(sid)
    try:
        os.stat(path)
    except FileNotFoundError:
        # Absent. The directory must still be usable, or a block now could not
        # be recorded and the next re-entry would see "absent" again.
        try:
            os.makedirs(_state_dir(), exist_ok=True)
        except OSError as e:
            return UNDET, f"marker directory {_state_dir()} is unusable ({e})"
        return ABSENT, ""
    except OSError as e:
        return UNDET, f"marker {path} could not be read ({e})"
    try:
        os.remove(path)
    except OSError as e:
        return UNDET, f"marker {path} exists but could not be cleared ({e})"
    return PRESENT, ""


def _record_block(payload) -> str | None:
    """Write this gate's marker after a block. Returns an error, or None."""
    sid = _session_id(payload)
    if sid is None:
        return "no usable session_id to key the marker on"
    try:
        os.makedirs(_state_dir(), exist_ok=True)
        with open(_marker_path(sid), "w", encoding="utf-8") as f:
            f.write("blocked\n")
    except OSError as e:
        return f"could not write {_marker_path(sid)} ({e})"
    return None


def _clear_marker(payload) -> None:
    sid = _session_id(payload)
    if sid is None:
        return
    try:
        os.remove(_marker_path(sid))
    except FileNotFoundError:
        pass
    except OSError as e:
        # A stale marker would grant one unchecked allow on a later re-entry;
        # say so rather than hide it.
        sys.stderr.write(
            f"stop-verify-worktree: could not clear marker ({e}); a later "
            "re-entry may be bounded-allowed without a check.\n")


def _allow_with_message(msg: str) -> int:
    sys.stdout.write(json.dumps({"systemMessage": msg}) + "\n")
    return 0


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except Exception:
        payload = {}
    reentry = isinstance(payload, dict) and bool(payload.get("stop_hook_active"))

    if reentry:
        sid = _session_id(payload)
        if sid is None:
            return _allow_with_message(
                "stop-verify-worktree: re-entrant stop with no usable session_id; "
                "cannot tell whether this gate blocked the previous stop, "
                + _UNVERIFIED)
        state, detail = _take_marker(sid)
        if state == PRESENT:
            return _allow_with_message(
                "stop-verify-worktree: bounded allow. This gate already blocked "
                "the previous stop of this session, so this re-entry is let "
                "through once without re-checking. The main-tree / deny-ledger "
                "violation may still be present (unverified, not clean).")
        if state == UNDET:
            return _allow_with_message(
                f"stop-verify-worktree: re-entrant stop, but {detail}; cannot "
                "tell whether this gate blocked the previous stop, "
                + _UNVERIFIED)
        # ABSENT: another gate blocked the previous stop. Run the full check.

    rc = _check(payload)
    if rc != 2:
        _clear_marker(payload)
        return rc
    err = _record_block(payload)
    if err is None:
        return rc
    if reentry:
        # Unrecorded block on a re-entry: every later re-entry would see "no
        # marker" and block again, an unbounded trap. Explicit allow instead.
        return _allow_with_message(
            f"stop-verify-worktree: this re-entrant stop would be blocked, but "
            f"the block marker could not be recorded ({err}); " + _UNVERIFIED)
    sys.stderr.write(
        f"\n(stop-verify-worktree: block marker not recorded: {err}.)\n")
    return rc


def _check(payload) -> int:
    """The deny-ledger and main-tree check: 0 allow, 2 block (stderr = why)."""
    if deny_ledger is None:
        sys.stderr.write(UNDETERMINED.format(
            what=f"loading scripts/deny_ledger.py ({_LEDGER_IMPORT_ERROR})"))
        return 2
    if isinstance(payload, dict):
        changed = deny_ledger.stop_check(payload)
        if changed is not None:
            sys.stderr.write(changed)
            return 2

    cwd = (payload.get("cwd") if isinstance(payload, dict) else None) or os.getcwd()

    st_git, git_dir = _probe(cwd, "rev-parse", "--absolute-git-dir")
    st_common, common = _probe(
        cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"
    )

    # Undetermined FIRST: if either probe could not answer, the NO_REPO reading
    # of the other one proves nothing, so it must not be allowed to win.
    if st_git == UNDET or st_common == UNDET:
        which = "git rev-parse --absolute-git-dir" if st_git == UNDET else (
            "git rev-parse --git-common-dir"
        )
        sys.stderr.write(UNDETERMINED.format(what=which))
        return 2
    if st_git == NO_REPO or st_common == NO_REPO:
        return 0  # git says there is no repository here — nothing to verify
    assert git_dir is not None and common is not None  # OK implies a value
    if os.path.realpath(git_dir) != os.path.realpath(common):
        return 0  # in a worktree — the sanctioned place

    # Main checkout: allowed only if clean.
    status = _git(cwd, "status", "--porcelain")
    if status is None or status.returncode != 0:
        sys.stderr.write(UNDETERMINED.format(what="git status --porcelain"))
        return 2
    if status.stdout.strip():
        sys.stderr.write(BLOCK)
        return 2
    return 0


if __name__ == "__main__":
    try:
        _rc = main()
    except Exception as _crash:  # noqa: BLE001
        # An uncaught exception exits 1, which Claude Code treats as a
        # non-blocking error, i.e. the call proceeds. A check that crashed has
        # not passed (CLAUDE.md 3), so it resolves to exit 2 (e033c406).
        sys.stderr.write(
            f"Do not stop yet: stop-verify-worktree.py crashed ({type(_crash).__name__}: {_crash}); "
            "a check that could not run has not passed.\n")
        _rc = 2
    sys.exit(_rc)
