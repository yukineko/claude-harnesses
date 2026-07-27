#!/usr/bin/env python3
"""PreToolUse hook: refuse a Bash command that MUTATES this project's main tree.

The Edit/Write tools are not the only way to change a file — `sed -i`, `rm`,
`mv`, `cp`, `tee`, a `>` redirection, `git apply`, `git checkout -- <path>` all
mutate the working tree from a Bash call, and guard-maintree-edit.py never sees
them. This hook closes those routes at edit time, for the main checkout of
$CLAUDE_PROJECT_DIR: a mutation whose resolved target lands under the main tree
(and is not git-ignored) is refused, with the same instruction — do it in a
worktree.

HONESTY ABOUT WHAT THIS IS (CLAUDE.md 4, and modelled on deny-no-verify.py). The
shell is Turing-complete; a sound "does this command mutate the main tree"
decision is not achievable from the command string. This hook is therefore a
HEURISTIC REDUCTION of the ways the main tree can be dirtied, NOT a proof that it
cannot be. Its backstop is the route-independent one: check-worktree-isolation.py
refuses any COMMIT taken from the main tree, so a mutation this hook fails to
catch still cannot become a durable, shared change on main. Known, unclosed
holes are listed at the bottom of this file rather than left for the next reader
to rediscover.

Resolution rule per candidate target:
  * relative paths resolve against the main tree root (the session's cwd);
  * a target that resolves OUTSIDE the main tree (a worktree, /tmp scratchpad,
    ~/.claude memory) is allowed;
  * a target under the main tree that is git-ignored is allowed (local scratch);
  * a target inside THIS process's OWN `.git/worktrees/<name>/` administrative
    directory (resolved from the hook's own cwd via `git rev-parse
    --absolute-git-dir`, never from the command string) is allowed — it is
    linked-worktree-private transient state (e.g. a stale `index.lock`), not
    main's tracked content, and is not shared with any other worktree/session;
  * a target under the main tree that is not ignored, and not inside that own
    admin dir, is REFUSED — this still covers `.git/config`, `.git/hooks/*`,
    `.git/refs/**`, and every OTHER worktree's `.git/worktrees/<other>/`.
When a command re-anchors into a worktree (`cd <wt> && …`, `git -C <wt> …`) the
command is allowed, since its mutations land in that worktree, not on main.

Undecidable input resolves to ALLOW here (not deny), deliberately, for the same
reason deny-no-verify.py exempts an unparseable payload: blocking every Bash call
that will not tokenize takes the whole turn down to protect a check that has a
working commit-time backstop. That exemption is a documented hole, not a claim of
safety.

    exit 0   allow
    exit 2   deny; stderr shown to the model
"""

from __future__ import annotations

import json
import os
import re
import shlex
import subprocess
import sys

REDIR = {">", ">>", ">|"}
REDIR_NUM = re.compile(r"^\d*>>?$")  # 1>, 2>>, >
SEPARATORS = frozenset({";", "&&", "||", "|", "&", ";;", "|&", "(", ")"})

# Commands where every non-flag operand is a filesystem target that gets created,
# overwritten, or removed. Over-inclusive on purpose (a refusal is cheap).
TARGET_ALL = {
    "rm", "unlink", "rmdir", "mv", "cp", "tee", "touch", "mkdir",
    "ln", "install", "truncate", "shred",
}


def _git(cwd: str, *args: str) -> str | None:
    try:
        out = subprocess.run(
            ("git", *args), cwd=cwd, capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.SubprocessError):
        return None
    if out.returncode != 0:
        return None
    return out.stdout.strip()


def _main_root() -> str | None:
    proj = os.environ.get("CLAUDE_PROJECT_DIR")
    if not proj:
        return None
    proj = os.path.realpath(proj)
    # Confirm it is a main checkout (git-dir == common-dir); if it is itself a
    # worktree or not a repo, we have no main tree to guard from here.
    gd = _git(proj, "rev-parse", "--absolute-git-dir")
    cm = _git(proj, "rev-parse", "--path-format=absolute", "--git-common-dir")
    if gd is None or cm is None or os.path.realpath(gd) != os.path.realpath(cm):
        return None
    return proj


def _under(child: str, parent: str) -> bool:
    parent = parent.rstrip("/")
    return child == parent or child.startswith(parent + "/")


def _resolve(root: str, path: str) -> str:
    if not os.path.isabs(path):
        path = os.path.join(root, path)
    return os.path.realpath(path)


_UNRESOLVABLE = set("$`*?{}~")


def _own_worktree_gitdir(root: str) -> str | None:
    """The absolute git-dir of the CALLING process's own worktree, i.e. the
    directory this hook is actually running from (cwd), resolved via git
    itself — NOT trusted from any string in the command being inspected.

    Returns None (undetermined) if this cannot be established, so the caller
    resolves undetermined to "not exempt" (CLAUDE.md 3: block side)."""
    gd = _git(os.getcwd(), "rev-parse", "--absolute-git-dir")
    if gd is None:
        return None
    gd = os.path.realpath(gd)
    # Only a genuine `.git/worktrees/<name>` administrative dir under THIS
    # main root qualifies — a bare repo's own .git, or a git-dir that is not
    # actually nested under root/.git/worktrees/, is not the carve-out this
    # exists for.
    worktrees_root = os.path.realpath(os.path.join(root, ".git", "worktrees"))
    if not _under(gd, worktrees_root) or gd == worktrees_root:
        return None
    return gd


def _hits_main(root: str, path: str, own_gitdir: str | None) -> bool:
    """True if `path` lands on a non-ignored location under the main tree."""
    if any(c in path for c in _UNRESOLVABLE):
        # A shell variable ($WT), glob, or brace-expansion — this process cannot
        # expand it, so it cannot be resolved to a literal path under the main
        # tree. Refusing it would false-positive on legitimate worktree flows
        # (`cd "$WT" && sed -i ...`); the commit chokepoint catches whatever this
        # lets through.
        return False
    resolved = _resolve(root, path)
    if not _under(resolved, root):
        return False
    # Narrow carve-out: the calling worktree's OWN `.git/worktrees/<name>/`
    # administrative directory (e.g. a stale index.lock) is not main's tracked
    # content and is not shared with any other worktree/session, so it is safe
    # to mutate. This is resolved from the hook's own cwd via git — never from
    # a string inside the candidate path — and is scoped to exactly one
    # worktree's admin dir, not a blanket `.git/` or `.git/worktrees/` allow
    # (main's own .git/config, .git/hooks/*, other worktrees' admin dirs, etc.
    # all remain protected below).
    if own_gitdir is not None and _under(resolved, own_gitdir):
        return False
    ignored = subprocess.run(
        ("git", "check-ignore", "-q", resolved), cwd=root, capture_output=True
    )
    return ignored.returncode != 0


def _tokenize(command: str) -> list[str] | None:
    lexer = shlex.shlex(command, posix=True, punctuation_chars="();<>|&\n\r")
    lexer.whitespace_split = True
    lexer.whitespace = lexer.whitespace.replace("\n", "").replace("\r", "")
    try:
        return [t.lstrip("\r\n") for t in lexer]
    except ValueError:
        return None


def _reanchored_outside(tokens: list[str], root: str) -> bool:
    """Command steps into a worktree (`cd <wt>` or `git -C <wt>`) whose path is
    not the main tree — its mutations land there, so allow."""
    for i, tok in enumerate(tokens):
        if tok == "cd" and i + 1 < len(tokens):
            dest = _resolve(root, tokens[i + 1])
            if not _under(dest, root):
                return True
        if tok == "-C" and i + 1 < len(tokens):
            dest = _resolve(root, tokens[i + 1])
            if not _under(dest, root):
                return True
    return False


def _candidate_targets(tokens: list[str], root: str) -> list[str]:
    """Filesystem targets this command would mutate (best effort)."""
    targets: list[str] = []

    # Redirection targets, anywhere in the command.
    for i, tok in enumerate(tokens):
        if (tok in REDIR or REDIR_NUM.match(tok)) and i + 1 < len(tokens):
            targets.append(tokens[i + 1])

    # Per-segment command analysis.
    segments: list[list[str]] = []
    cur: list[str] = []
    for tok in tokens:
        if tok in SEPARATORS:
            if cur:
                segments.append(cur)
            cur = []
        elif tok in REDIR or REDIR_NUM.match(tok):
            cur.append("\0redir")  # placeholder so the next token is skipped
        else:
            cur.append(tok)

    if cur:
        segments.append(cur)

    for seg in segments:
        # Drop redirection targets already captured, and env assignments.
        argv = [t for t in seg if t != "\0redir"]
        k = 0
        while k < len(argv) and "=" in argv[k] and not argv[k].startswith("-"):
            k += 1
        argv = argv[k:]
        if not argv:
            continue
        prog = argv[0].split("/")[-1]
        rest = argv[1:]

        if prog in TARGET_ALL:
            targets += [a for a in rest if not a.startswith("-")]
        elif prog in ("sed", "gsed") and any(
            a == "-i" or a.startswith("-i") for a in rest
        ):
            ops = [a for a in rest if not a.startswith("-")]
            # `sed -i 's/a/b/' f1 f2` — the FIRST operand is the script, not a
            # file, unless the script came via -e/-f (then all operands are
            # files). Treating the script as a path resolved it under the main
            # root and wrongly refused a legitimate worktree sed.
            has_expr = any(
                a in ("-e", "-f") or a.startswith("-e") or a.startswith("-f")
                for a in rest
            )
            targets += ops if has_expr else ops[1:]
        elif prog in ("perl", "ruby") and any(
            a == "-i" or a.startswith("-i") for a in rest
        ):
            # perl/ruby one-liners carry the script in -e/-pe, so operands are
            # files.
            targets += [a for a in rest if not a.startswith("-")]
        elif prog == "dd":
            targets += [a[3:] for a in rest if a.startswith("of=")]
        # NOTE: git subcommands (rm/mv/apply/checkout/restore/stash/reset/clean)
        # are deliberately NOT handled here. Their effect depends on the cwd they
        # run in (often a worktree), they are frequently RECOVERY or move-to-
        # worktree operations (`git stash`, `git checkout -- <path>` discard
        # changes — they clean the tree, they do not add code to it), and an
        # over-match here false-positived on the sanctioned worktree flow
        # (`git stash push` + `git worktree add`). Any git mutation that actually
        # reaches main is caught by check-worktree-isolation.py at commit time,
        # which is the sound, route-independent gate. `patch` is likewise left to
        # the commit chokepoint rather than over-matched to the whole tree.

    return targets


DENY = """Refused: `{cmd}` mutates this project's MAIN working tree.

CLAUDE.md 最上位の方針 8: nothing edits, adds to, or deletes from the main tree
directly — every mutation happens in a `git worktree` and reaches main through a
merge. Another session is always assumed to be sharing this index.

    git worktree add -b <branch> <path> <base>
    # run the mutation against files inside that worktree, commit, then merge.
"""


def decide(payload: dict) -> tuple[int, str]:
    if payload.get("tool_name") != "Bash":
        return 0, ""
    command = (payload.get("tool_input") or {}).get("command")
    if not isinstance(command, str) or not command.strip():
        return 0, ""

    root = _main_root()
    if root is None:
        # No identifiable main tree to protect from this process -> nothing to do.
        return 0, ""

    tokens = _tokenize(command)
    if tokens is None:
        # Undecidable; see module docstring. Commit-time backstop holds.
        return 0, ""

    if _reanchored_outside(tokens, root):
        return 0, ""

    own_gitdir = _own_worktree_gitdir(root)
    for target in _candidate_targets(tokens, root):
        if _hits_main(root, target, own_gitdir):
            return 2, DENY.format(cmd=command.strip().splitlines()[0][:120])
    return 0, ""


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError):
        return 0
    if not isinstance(payload, dict):
        return 0
    code, reason = decide(payload)
    if code != 0:
        sys.stderr.write(reason)
    return code


if __name__ == "__main__":
    sys.exit(main())

# KNOWN, UNCLOSED HOLES (recorded, not hidden — CLAUDE.md 4):
#   * interpreter wrappers hide the mutation: `sh -c '…'`, `bash -lc`, `eval`,
#     `python3 -c "open(p,'w')"`, `xargs`, `env`, `nohup`, `time` — argv[0] is
#     not a known mutator, so the command is allowed.
#   * a `cd <worktree>` earlier in the command re-anchors and allows the WHOLE
#     command, including a later `>` back into the main tree by absolute path.
#   * moving/copying INTO the main tree from an outside source is caught (dest is
#     scanned) but a tool that resolves its own paths (a Makefile, cargo) is not.
#   All of these are caught at the durable moment by check-worktree-isolation.py,
#   which refuses the commit regardless of how the tree was dirtied.
