#!/usr/bin/env python3
"""PreToolUse hook: refuse an Edit/Write to THIS project's MAIN working tree.

CLAUDE.md 最上位の方針 8 forbids editing the shared main working tree directly —
every edit must happen in a `git worktree`, because two sessions sharing one
index is the single conflict git cannot resolve. That rule used to live only in
prose, and prose is a request, not an enforcement: the implementer (§6, and an
LLM most of all) drifts to "just this one small edit on main is fine". This hook
makes the drift impossible for the common edit path — the tool call passes
through this process before any file is touched, so a main-tree edit can be
REFUSED outright rather than merely discouraged.

Scope, stated precisely so the ALLOW paths are not accidents:

  * Only the EDIT tools are inspected — Edit / Write / MultiEdit / NotebookEdit.
    Every other tool exits 0 (allow).
  * Only THIS project's main checkout is guarded. Its location is the parent
    of the git common dir of $CLAUDE_PROJECT_DIR (or, unset, of the payload
    cwd) — a location lookup, not a read of main's state.
  * OWN TREE ONLY (user ruling 2026-11-08, verbatim: "donegateそのものが他の
    worktreeを見たり検証したりテストしたりすることは禁止とする。他のgateでも禁止
    とする。合理的理由があって確認するなら、それを提示すること。基本的に認めない"
    / "おそらく他のworktreeへの干渉をやめないと並行性があがるほど、処理する時間が
    膨大になる、そこに合理的理由はない。定期的にまとまったauditの実施での担保と
    する。"). The session's own tree is the tree its payload cwd is in; the
    tree a TARGET belongs to is judged by its path alone:
      - a target under main's directory, not below a nested `.git` entry, is
        in main. If the cwd is NOT main it is refused by path; no git runs in
        main and main's MERGE_HEAD is not read (so a merge in progress in main
        does not unlock it). If the cwd IS main, main is the session's own tree
        and the rules below (gitignored allow, merge-in-progress allow) apply,
        reading main's own state;
      - a target in any linked worktree (nested under main below a `.git`
        entry, or outside it) is ALLOWED, without reading that tree's state or
        the worktree registration list.
    Consequence, stated plainly: a gitignored main path is refused unless the
    cwd is main (its ignore status is main's state).
  * Files OUTSIDE the project main tree are allowed: the memory dir under
    ~/.claude, the session scratchpad under /tmp, any other repo. Those are not
    the shared index this rule protects.
  * MERGE IN PROGRESS (backlog c8c11add, user ruling 2026-10-04), cwd main
    only: while `MERGE_HEAD` exists in the main checkout's git dir, an edit to
    ANY path in the main tree is ALLOWED and its target appended to this
    session's write record (deny_ledger.record_writes; a record that cannot be
    written refuses the edit) so the Stop gate can attribute it — conflict
    resolution is the integration step §8
    permits on main, and the commit-side guard (check-worktree-isolation.py)
    already allows the commit whenever MERGE_HEAD exists. The probe mirrors that
    guard's bounded retry for the marker's visibility lag. Only MERGE_HEAD
    counts: REBASE_HEAD / CHERRY_PICK_HEAD alone stay refused. A MERGE_HEAD that
    exists but is not a readable regular file (a directory, a symlink, an
    unreadable file, an lstat error) is undetermined and refused (CLAUDE.md 3).
    The hook-machinery and deny-ledger-directory refusals below are checked
    first and are NOT lifted by a merge in progress.
  * git-ignored paths under the main tree are allowed (personal, uncommitted
    scratch such as settings.local.json). They never enter the shared history,
    so editing them on main creates none of the index-sharing harm.

Fail-closed (CLAUDE.md 3): a target that IS under the project main tree but whose
worktree status cannot be determined (a git call errors unexpectedly) resolves to
DENY, not allow. "Cannot determine whether this is main" is not "this is safe".
A payload that does not decode as a JSON object is likewise refused (exit 2):
the deny ledger below cannot be consulted for a call it cannot read.

HOOK MACHINERY (e033c406, second gate). Before any of the above, an edit whose
target — as written or after realpath — is inside `.git/hooks`, is a
`.git/config`, or is a `config.worktree` under `.git` is refused, in ANY tree
(main, a worktree, another repo) and regardless of CLAUDE_PROJECT_DIR: those
files are git's own untracked hook/config plumbing (no reviewable commit), and
rewriting them disarms every gate at once.

The tracked `.githooks` directory is NOT in that set (user rulings 2026-10-03
"refusing edits/deletes in a worktree is itself the defect" and 2026-10-04
"remove what stops wiring into .githooks"). An edit to `.githooks` inside a
linked worktree is ordinary, reviewable work and is allowed like any other
worktree file. An edit to the `.githooks` of a PRIMARY checkout (a git work
tree whose `.git` is a directory — the main tree) is still refused, in ANY
primary checkout and regardless of CLAUDE_PROJECT_DIR (so a session anchored on
a worktree cannot reach main's `.githooks` either). The path is judged both as
written and by its realpath, so a worktree path that symlinks into main's
`.githooks` is refused the same way. A `.git` beside the `.githooks` that
cannot be stat'ed is undetermined and refused. Only a merge in progress in
that checkout (a readable MERGE_HEAD, as in the main-tree rule) lifts this
refusal, and only when the payload cwd is in that very checkout (its own
tree); otherwise its MERGE_HEAD is not read (user ruling 2026-11-08). An
unreadable MERGE_HEAD is refused. Likewise an edit whose realpath is inside either deny
ledger directory (`~/.claude/state/maintree-deny`, `<tmp>/maintree-deny-<uid>`)
is refused in any tree: editing it would erase or forge the record below.
There are no twin rules for Bash: guard-maintree-bash.py observes main's state
instead of parsing commands, and does not record into or consult the ledger
(user ruling 2026-10-04).

DENY LEDGER (e033c406, scripts/deny_ledger.py). Every refusal is appended to the
per-session ledger `~/.claude/state/maintree-deny/<session_id>.jsonl` — or, when that cannot be
written, the temp-dir fallback `<tmp>/maintree-deny-<uid>/<session_id>.jsonl`
(both are always read) — target, `raw` truncated to 120 characters,
the reason's first line truncated to 300, and a snapshot of the target's state.
A refusal that can be written to neither location keeps its deny and says so.
A call this guard's own rules
ALLOW is then checked against that ledger: if it names a target refused earlier
in the same session (within 25 guarded calls / 20 minutes), it is refused as an
ask quoting the earlier refusal — hardened to a deny unless the session is an
interactive terminal (CLAUDECODE=1 and CLAUDE_CODE_ENTRYPOINT=cli). An
unreadable or corrupt ledger, or an unusable session_id, refuses the same way —
a corrupt (but readable) ledger file only for 20 minutes from its mtime, after
which it is renamed aside to `<name>.corrupt-<ts>` (or `-<n>` suffixed when that name
exists; an aside is never replaced), its valid deny lines are
carried into a fresh ledger file, the unparseable lines are dropped (named in a
stderr notice) and the call proceeds. An UNREADABLE ledger file (e.g. mode 000)
has no such bound and is never moved aside: nothing can be salvaged from it, so
every guarded call is refused until it is repaired, removed or cleared by the
next user prompt (issue #1182). See deny_ledger.py for the exact rules and residuals.

Protocol: reads the PreToolUse JSON payload on stdin.

    exit 0   allow (or, with a permissionDecision "ask" JSON on stdout, ask)
    exit 2   deny; stderr is shown to the model as the reason
"""

from __future__ import annotations

import json
import os
import stat
import subprocess
import sys
import time

try:
    # A failed import must not crash the hook (exit 1 is a non-blocking error,
    # i.e. an allow); it resolves to a deny in main().
    sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
    import deny_ledger  # noqa: E402
    _LEDGER_IMPORT_ERROR = ""
except Exception as _e:  # noqa: BLE001
    deny_ledger = None
    _LEDGER_IMPORT_ERROR = f"{type(_e).__name__}: {_e}"

EDIT_TOOLS = ("Edit", "Write", "MultiEdit", "NotebookEdit")


def _target_path(payload: dict) -> str | None:
    """The absolute filesystem path an edit tool would write, or None."""
    ti = payload.get("tool_input") or {}
    # Edit/Write/MultiEdit use file_path; NotebookEdit uses notebook_path.
    for key in ("file_path", "notebook_path"):
        val = ti.get(key)
        if isinstance(val, str) and val:
            return val
    return None


def _nearest_existing_dir(path: str) -> str:
    """Walk up to the nearest existing ancestor dir (a new file's parent may not
    exist yet). Absolute input guaranteed by the caller."""
    d = os.path.dirname(path) or path
    while d and d != os.path.dirname(d) and not os.path.isdir(d):
        d = os.path.dirname(d)
    return d or "/"


def _git(cwd: str, *args: str) -> str | None:
    """Run a git query in cwd; return stripped stdout, or None on any failure."""
    try:
        out = subprocess.run(
            ("git", *args),
            cwd=cwd,
            capture_output=True,
            text=True,
            timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    return out.stdout.strip()


def _under(child: str, parent: str) -> bool:
    parent = parent.rstrip("/")
    return child == parent or child.startswith(parent + "/")


_FOLD_CASE = sys.platform == "darwin"


def _hook_protected(path: str) -> bool:
    """Inside `.git/hooks`, a `.git/config`, or a `config.worktree` under
    `.git`. Unlike guard-maintree-bash.py's shape rule this deliberately does
    NOT include `.githooks` (user rulings 2026-10-03/04): the tracked
    `.githooks` is editable in a linked worktree, and a primary checkout's copy
    is refused by _primary_githooks_root in _judge."""
    p = path.casefold() if _FOLD_CASE else path
    comps = [c for c in p.split("/") if c]
    for i, c in enumerate(comps):
        if c == ".git" and i + 1 < len(comps):
            if comps[i + 1] == "hooks":
                return True
            if comps[i + 1] == "config" and i + 2 == len(comps):
                return True
        if c == "config.worktree" and ".git" in comps[:i]:
            return True
    return False


def _primary_githooks_root(path: str) -> tuple[str, str | None]:
    """Is `path` inside the `.githooks` of a PRIMARY checkout?

    For every `.githooks` component, the directory P holding it is a primary
    checkout when `P/.git` is a directory (a linked worktree's `.git` is a
    file). Returns ("primary", P) for the first such P, ("undetermined", P)
    when `P/.git` exists-or-not cannot be told (an lstat/stat error other than
    not-found), and ("none", None) otherwise."""
    comps = [c for c in os.path.normpath(path).split("/") if c]
    for i, c in enumerate(comps):
        if (c.casefold() if _FOLD_CASE else c) != ".githooks":
            continue
        parent = "/" + "/".join(comps[:i])
        try:
            st = os.stat(os.path.join(parent, ".git"))
        except (FileNotFoundError, NotADirectoryError):
            continue
        except OSError:
            return "undetermined", parent
        if stat.S_ISDIR(st.st_mode):
            return "primary", parent
    return "none", None


def payload_cwd(payload: dict) -> str | None:
    cwd = payload.get("cwd")
    return cwd if isinstance(cwd, str) and os.path.isabs(cwd) else None


def _cwd_tree(payload: dict) -> tuple[str, str, str] | None:
    """(toplevel, git dir, common dir) realpaths of the tree the payload's cwd
    is in — the session's OWN tree — or None when cwd is missing / not a
    directory / not inside a git work tree / git cannot answer. None is never
    "main": a target in main is then refused (restrictive side)."""
    cwd = payload_cwd(payload)
    if cwd is None or not os.path.isdir(cwd):
        return None
    out = _git(cwd, "rev-parse", "--path-format=absolute", "--show-toplevel",
               "--absolute-git-dir", "--git-common-dir")
    if out is None:
        return None
    lines = out.splitlines()
    if len(lines) != 3 or not all(lines):
        return None
    top, gd, common = (os.path.realpath(x) for x in lines)
    return top, gd, common


def _nested_tree(target: str, main_root: str) -> str:
    """Does `target` (under main_root) belong to a tree nested inside main —
    a linked worktree or submodule checked out under main's directory? Judged
    by path alone: some directory strictly between main_root and the target
    holds a `.git` entry. "nested" / "main" / "undetermined" (an lstat error
    other than not-found). No git runs in that other tree and its state is not
    read (user ruling 2026-11-08)."""
    d = os.path.dirname(target)
    root = main_root.rstrip("/")
    while _under(d, root) and d != root:
        try:
            os.lstat(os.path.join(d, ".git"))
            return "nested"
        except (FileNotFoundError, NotADirectoryError):
            pass
        except OSError:
            return "undetermined"
        d = os.path.dirname(d)
    return "main"


def decide(payload: dict) -> tuple[int, str]:
    """(exit_code, stderr). 0 allow, 2 deny."""
    code, reason, _meta = _judge(payload)
    return code, reason


def _judge(payload: dict) -> tuple[int, str, dict]:
    """(exit_code, stderr, meta); meta = {target_abs, root} for the ledger."""
    meta: dict = {"target_abs": None, "root": None}
    if payload.get("tool_name") not in EDIT_TOOLS:
        return 0, "", meta

    raw = _target_path(payload)
    if raw is None:
        # An edit tool with no resolvable path: cannot locate it, cannot clear
        # it. This hook is the edit-time gate, so cannot-determine -> deny (3.).
        return 2, DENY_UNDETERMINED, meta

    target = os.path.realpath(raw)
    if _hook_protected(raw) or _hook_protected(target):
        return 2, DENY_HOOKS.format(path=raw), meta
    # The deny ledger's own directories, in any tree (e033c406 round 3).
    for d in (deny_ledger.protected_dirs() if deny_ledger is not None else []):
        if _under(*(x.casefold() if _FOLD_CASE else x for x in (target, d))):
            return 2, DENY_LEDGER_DIR.format(path=raw, dir=d), meta

    # The session's own tree is the tree its cwd is in. Only that tree's state
    # may be read (user ruling 2026-11-08); every other tree is judged by path.
    own = _cwd_tree(payload)

    # A primary checkout's `.githooks`, in any repo and under any project
    # anchor, judged as written and by realpath.
    for cand in (os.path.abspath(raw), target):
        kind, root = _primary_githooks_root(cand)
        if kind == "none":
            continue
        meta["target_abs"] = target
        meta["root"] = os.path.realpath(root)
        if kind == "undetermined":
            return 2, DENY_UNDETERMINED, meta
        if own is None or own[0] != meta["root"] or own[1] != own[2]:
            # Another tree's primary checkout: refused by path; its MERGE_HEAD
            # is not read.
            return 2, DENY_PRIMARY_GITHOOKS.format(path=raw, root=root), meta
        merge = _merge_head_state(os.path.realpath(os.path.join(root, ".git")))
        if merge == "undetermined":
            return 2, DENY_MERGE_HEAD_UNDETERMINED.format(
                path=raw, marker=os.path.join(root, ".git", "MERGE_HEAD")), meta
        if merge != "present":
            return 2, DENY_PRIMARY_GITHOOKS.format(path=raw, root=root), meta
    meta = {"target_abs": None, "root": None}

    proj_env = os.environ.get("CLAUDE_PROJECT_DIR")
    anchor = proj_env or payload_cwd(payload)
    if not anchor:
        return 2, DENY_UNDETERMINED, meta
    anchor = os.path.realpath(anchor)
    # Where this project's MAIN tree is: the parent of the anchor's git common
    # dir. A location lookup only (rev-parse at the anchor); no state of main
    # or of any other tree is read.
    common_out = _git(anchor, "rev-parse", "--path-format=absolute", "--git-common-dir") \
        if os.path.isdir(anchor) else None
    if common_out is None:
        # The anchor is in no repo (or git is unusable there): a target under
        # it cannot be cleared, anything else is outside this project.
        if _under(target, anchor):
            return 2, DENY_UNDETERMINED, meta
        return 0, "", meta
    common_real = os.path.realpath(common_out)
    if os.path.basename(common_real) != ".git":
        return 0, "", meta  # a bare repository has no main working tree
    main_root = os.path.dirname(common_real)
    if not _under(target, main_root):
        # Outside main: a linked worktree outside the repo (ALLOWED without
        # reading its state), memory, scratchpad, another repo.
        return 0, "", meta
    nested = _nested_tree(target, main_root)
    if nested == "undetermined":
        return 2, DENY_UNDETERMINED, meta
    if nested == "nested":
        return 0, "", meta  # a nested linked worktree / submodule: another tree
    if own is None or own[0] != main_root or own[1] != own[2]:
        # Main is not this session's tree: refused by path alone. main's
        # MERGE_HEAD is NOT read, so a merge in progress in main does not
        # unlock it (resolve conflicts from a session whose cwd is main).
        meta["target_abs"] = target
        meta["root"] = main_root
        return 2, DENY_MAINTREE_NOT_OWN.format(path=raw, main=main_root), meta
    proj = main_root

    # From here main IS the session's own tree (cwd is main), so its state
    # may be read: the pre-2026-11-08 rules apply unchanged.
    query_dir = _nearest_existing_dir(target)
    git_dir = _git(query_dir, "rev-parse", "--absolute-git-dir")
    common = _git(query_dir, "rev-parse", "--path-format=absolute", "--git-common-dir")

    if git_dir is None or common is None:
        # The file is not inside any git repo (or git is unusable here). If it is
        # nonetheless under the project root, we cannot confirm it is a worktree
        # -> fail closed. If proj is unknown, a non-repo path is genuinely
        # outside our concern -> allow.
        if proj is not None and _under(target, proj):
            return 2, DENY_UNDETERMINED, meta
        return 0, "", meta

    is_main_checkout = os.path.realpath(git_dir) == os.path.realpath(common)
    if not is_main_checkout:
        # Linked worktree — exactly where edits are supposed to happen.
        return 0, "", meta

    # Main checkout. When we have a project anchor, only guard THIS project's
    # main tree; a different repo's main tree is not ours to police here.
    toplevel = _git(query_dir, "rev-parse", "--show-toplevel")
    if proj is not None:
        if toplevel is not None and os.path.realpath(toplevel) != proj:
            return 0, "", meta

    # git-ignored scratch under the main tree never enters shared history.
    ignored = subprocess.run(
        ("git", "check-ignore", "-q", target),
        cwd=query_dir,
        capture_output=True,
    )
    if ignored.returncode == 0:
        return 0, "", meta

    meta["target_abs"] = target
    meta["root"] = proj if proj is not None else (
        os.path.realpath(toplevel) if toplevel is not None else None)

    # Integration carve-out (backlog c8c11add): while a merge is in progress on
    # main, editing main's tree IS the allowed operation (conflict resolution).
    merge = _merge_head_state(git_dir)
    if merge == "present":
        # An allowed write into main by THIS session: it goes into the
        # session's write record so the Stop gate can attribute it (main()).
        meta["write"] = True
        return 0, "", meta
    if merge == "undetermined":
        return 2, DENY_MERGE_HEAD_UNDETERMINED.format(
            path=raw, marker=os.path.join(git_dir, "MERGE_HEAD")), meta
    return 2, DENY_MAINTREE.format(path=raw), meta


# Same bounded retry budget as check-worktree-isolation.py's
# MERGE_HEAD_RETRY_* (the visibility lag between git writing MERGE_HEAD and a
# hook observing it). Only an ABSENT marker is retried; giving up answers
# "absent" (-> deny), never "present".
MERGE_HEAD_RETRY_ATTEMPTS = 5
MERGE_HEAD_RETRY_DELAY_S = 0.1


def _merge_head_state(git_dir: str) -> str:
    """Tri-state MERGE_HEAD probe in the main checkout's git dir.

    "present"      MERGE_HEAD is a regular file this process can open and read.
    "absent"       MERGE_HEAD does not exist (after the bounded retry).
    "undetermined" anything else: a directory, a symlink or other non-regular
                   file, an unreadable file, or an lstat error other than
                   ENOENT. Resolves to deny (CLAUDE.md 3).

    Only MERGE_HEAD counts. REBASE_HEAD / CHERRY_PICK_HEAD are deliberately not
    consulted: the user ruling (2026-10-04) scopes the carve-out to the same
    signal the commit-side guard honours.
    """
    marker = os.path.join(git_dir, "MERGE_HEAD")
    for attempt in range(MERGE_HEAD_RETRY_ATTEMPTS):
        try:
            st = os.lstat(marker)
        except FileNotFoundError:
            if attempt < MERGE_HEAD_RETRY_ATTEMPTS - 1:
                time.sleep(MERGE_HEAD_RETRY_DELAY_S)
            continue
        except OSError:
            return "undetermined"
        if not stat.S_ISREG(st.st_mode):
            return "undetermined"
        try:
            with open(marker, "rb") as f:
                f.read(1)
        except OSError:
            return "undetermined"
        return "present"
    return "absent"


DENY_MAINTREE = """Refused: editing `{path}` writes to this project's MAIN working tree.

CLAUDE.md 最上位の方針 8: the main working tree is never edited directly — every
edit goes through a `git worktree`, because a shared index is the one conflict
git cannot merge, and another session is ALWAYS assumed to be live. This is not a
suggestion the hook is reminding you of; it is enforced here in code.

Do this instead:

    git worktree add -b <branch> <path-outside-or-nested> <base>

then make the edit against the file inside that worktree, commit there, and merge
into main. Merge / conflict-resolution are the ONLY operations allowed on the
main tree: while a merge is in progress on main (a readable MERGE_HEAD regular
file in main's git dir) edits to main's tree are allowed so the conflicts can be
resolved. A rebase or cherry-pick in progress (REBASE_HEAD / CHERRY_PICK_HEAD)
does not unlock this.
"""

DENY_MAINTREE_NOT_OWN = """Refused: editing `{path}` writes to this project's MAIN working tree, and main is not this session's tree.

The session's cwd is not main (`{main}`), so main is another tree. Under the
user ruling of 2026-11-08 a gate judges only its own tree and does not read
another tree's state, so this edit is refused by its path alone: a merge in
progress in main is NOT consulted and does not unlock it. Make the edit in your
worktree and merge it; resolve main's conflicts from a session whose cwd is main.
"""

DENY_WRITE_RECORD = """Refused: editing `{path}` (main, merge in progress) could not be recorded in this session's write record: {why}.

The Stop gate attributes uncommitted main-tree paths to this session only
through that record; an unrecorded write could not be attributed, and a check
that could not run has not passed (CLAUDE.md 3).
"""

DENY_MERGE_HEAD_UNDETERMINED = """Refused: editing `{path}` writes to this project's MAIN working tree, and
whether a merge is in progress could not be determined.

`{marker}` exists but is not a readable regular file, so this guard cannot tell
"conflict resolution of a merge in progress" (allowed) from "an ordinary edit on
main" (refused). Under CLAUDE.md 3 cannot-determine resolves to the restricted
side. Inspect that path; if no merge is in progress, make the edit in a worktree.
"""

DENY_PRIMARY_GITHOOKS = """Refused: editing `{path}` writes to the `.githooks` of a MAIN working tree (`{root}`).

`{root}` is a primary checkout (its `.git` is a directory). CLAUDE.md 最上位の方針 8:
the main working tree is never edited directly, and its tracked `.githooks` is
what `core.hooksPath` runs for every commit. This is refused whatever
CLAUDE_PROJECT_DIR is, and the path is judged by its realpath too, so a symlink
from a worktree into it is refused the same way. Edit `.githooks` inside a
linked worktree and commit it there; only a merge in progress in that checkout
(a readable MERGE_HEAD) lifts this.
"""

DENY_HOOKS = """Refused: editing `{path}` would rewrite this repository's git hook machinery.

The local gates are wired through `core.hooksPath` in `.git/config` and git's
own `.git/hooks` (CLAUDE.md 最上位の方針 7). Those are untracked plumbing:
writing into `.git/hooks`, `.git/config` or a `config.worktree` can disarm every
gate at once without leaving a reviewable commit, so it is refused in any tree
(backlog e033c406). Reading them is allowed. If they genuinely need to change,
hand it to the human. (The tracked `.githooks` directory is different: edit it
inside a linked worktree and commit it there.)
"""

DENY_LEDGER_DIR = """Refused: editing `{path}` writes into the maintree deny ledger directory `{dir}`.

That directory records this session's refusals (backlog e033c406); editing it
would erase or forge the record that the retry and Stop checks rely on. It is
refused from any tree. Reading it is allowed; a new user prompt clears it.
"""

DENY_BAD_PAYLOAD = """Refused: could not read the hook payload ({why}).

A payload this gate could not read is not an edit it checked, and the
per-session deny ledger cannot be consulted for it (CLAUDE.md 最上位の方針 3).
"""

DENY_LEDGER_MODULE = """Refused: the maintree deny ledger module could not be loaded ({why}).

scripts/deny_ledger.py records this session's refusals so the same target cannot
be re-reached by another spelling (backlog e033c406). Without it that check
cannot run, and a check that could not run has not passed (CLAUDE.md 3).
"""

DENY_UNDETERMINED = """Refused: could not determine whether this edit targets the main working tree.

A check that could not run has not passed. Under CLAUDE.md 3 (cannot-determine
resolves to the restricted side), an edit whose worktree status is unknown is
refused rather than allowed. Make the edit inside a git worktree and it will be
permitted.
"""


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as e:
        sys.stderr.write(DENY_BAD_PAYLOAD.format(why=f"not JSON: {e}"))
        return 2
    if not isinstance(payload, dict):
        sys.stderr.write(DENY_BAD_PAYLOAD.format(
            why=f"JSON {type(payload).__name__}, not an object"))
        return 2
    if deny_ledger is None:
        sys.stderr.write(DENY_LEDGER_MODULE.format(why=_LEDGER_IMPORT_ERROR))
        return 2
    code, reason, meta = _judge(payload)
    if code != 0:
        note = deny_ledger.record_deny(
            payload, "guard-maintree-edit.py", meta["target_abs"], meta["root"],
            f"{payload.get('tool_name')} {_target_path(payload)}", reason,
            raw_target=_target_path(payload))
        sys.stderr.write(reason + (("\n" + note + "\n") if note else ""))
        return code
    if payload.get("tool_name") not in EDIT_TOOLS:
        return 0
    # This guard's own judgement allowed the edit; now the deny ledger
    # (signal 1): the same session re-reaching a refused target.
    rc = deny_ledger.gate(payload)
    if rc is not None:
        return rc
    if meta.get("write"):
        # An allowed edit of main by this session (cwd main, merge in
        # progress): unrecorded, it could not be attributed at Stop, so a
        # write that cannot be recorded is refused (CLAUDE.md 3).
        err = deny_ledger.record_writes(payload, meta["root"], [meta["target_abs"]],
                                        "guard-maintree-edit.py")
        if err is not None:
            sys.stderr.write(DENY_WRITE_RECORD.format(path=_target_path(payload), why=err))
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
            f"Refused: guard-maintree-edit.py crashed ({type(_crash).__name__}: {_crash}); "
            "a check that could not run has not passed.\n")
        _rc = 2
    sys.exit(_rc)
