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
  * Only THIS project's main checkout is guarded. "This project" is
    realpath($CLAUDE_PROJECT_DIR); "main checkout" is where the target file's
    `--git-dir` equals its `--git-common-dir` (a linked worktree's git-dir lives
    under `<common>/worktrees/<name>`, so they differ there). A file in a
    worktree — nested inside the repo or outside it — is therefore ALLOWED, which
    is the whole point: work in a worktree and this hook is silent.
  * Files OUTSIDE the project main tree are allowed: the memory dir under
    ~/.claude, the session scratchpad under /tmp, any other repo. Those are not
    the shared index this rule protects.
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
worktree file. An edit to the MAIN tree's `.githooks` is still refused — by the
general main-tree rule above (it is tracked, so not git-ignored), with the
main-tree message — and a worktree path whose realpath lands in main's
`.githooks` (a symlink) is judged by that realpath and refused the same way. Likewise an edit whose realpath is inside either deny
ledger directory (`~/.claude/state/maintree-deny`, `<tmp>/maintree-deny-<uid>`)
is refused in any tree: editing it would erase or forge the record below. The
twin rules for Bash live in guard-maintree-bash.py.

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
a corrupt or unreadable ledger file only for 20 minutes from its mtime, after
which it is renamed aside to `<name>.corrupt-<ts>`, its valid deny lines are
carried into a fresh ledger file, the unparseable lines are dropped (named in a
stderr notice) and the call proceeds. See deny_ledger.py for the exact rules and residuals.

Protocol: reads the PreToolUse JSON payload on stdin.

    exit 0   allow (or, with a permissionDecision "ask" JSON on stdout, ask)
    exit 2   deny; stderr is shown to the model as the reason
"""

from __future__ import annotations

import json
import os
import subprocess
import sys

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
    `.githooks` is editable in a linked worktree, and main's copy is refused by
    the main-tree rule in _judge."""
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

    proj_env = os.environ.get("CLAUDE_PROJECT_DIR")
    if not proj_env:
        # Without the project anchor we cannot scope "this project's main tree".
        # Fall back to the intrinsic test (git-dir == git-common-dir) so the hook
        # still enforces, rather than silently allowing everything.
        proj = None
    else:
        proj = os.path.realpath(proj_env)
        if not _under(target, proj):
            # Outside this project entirely (memory, scratchpad, other repo).
            return 0, "", meta

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
    return 2, DENY_MAINTREE.format(path=raw), meta


DENY_MAINTREE = """Refused: editing `{path}` writes to this project's MAIN working tree.

CLAUDE.md 最上位の方針 8: the main working tree is never edited directly — every
edit goes through a `git worktree`, because a shared index is the one conflict
git cannot merge, and another session is ALWAYS assumed to be live. This is not a
suggestion the hook is reminding you of; it is enforced here in code.

Do this instead:

    git worktree add -b <branch> <path-outside-or-nested> <base>

then make the edit against the file inside that worktree, commit there, and merge
into main. Merge / conflict-resolution are the ONLY operations allowed on the
main tree.
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
    return 0 if rc is None else rc


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
