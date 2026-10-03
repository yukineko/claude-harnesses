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
A `cd <dir>` re-anchors RELATIVE targets of the segments after it (so
`cd <wt> && sed -i … f` resolves `f` in the worktree); an absolute target that
still names main is judged as such, and a `( … )` subshell's `cd` does not leak
out. The judgement follows the EFFECT, not argv[0] (backlog ae4543d5):
`sh -c` / `bash -lc` / `eval` payloads are re-tokenized and judged by these same
rules, `env` / `nohup` / `time` / `nice` / `sudo` prefixes are peeled off, and
inline interpreter programs (`python -c`, `node -e`, `ruby -e`, `perl -e`, `awk`)
that write, delete, or spawn a shell make their path literals targets — with no
nameable path, that is "cannot determine" and is refused under main.

UNDECIDABLE INPUT RESOLVES TO DENY (CLAUDE.md 3), as it always has in the twin
guard-maintree-edit.py. Three sites used to answer "I could not tell" with ALLOW:
the main root could not be established, the command would not tokenize, and the
target contained a shell construct this process cannot expand. The middle one was
not theoretical — a here-document whose body contains an apostrophe does not
tokenize, so `cat > <main>/f <<'EOF' … it's … EOF` was waved straight through.

Rather than pay for that in false positives, the guard first makes MORE input
decidable, and only then denies what is left:

  * here-document BODIES are stripped before tokenizing (they are data, not
    shell syntax) — but only when the terminator is actually found, so a `<<`
    inside a quoted string cannot swallow later lines;
  * `~`, `$HOME` and `$PWD` are expanded, because they are deterministic;
  * for a target still holding `$`, a glob or a brace, the longest LITERAL path
    prefix is resolved: if that prefix and the main root are on the same ancestor
    chain the expansion could land on main, so it is refused; if they are on
    disjoint branches it provably cannot, so it is allowed.

The remaining refusals are honest "cannot determine" answers, and each names what
could not be resolved so the caller can rewrite it with a literal path.

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


# Probe outcomes. Three answers, not two (crates/blastguard/src/model.rs,
# harness_core::verdict). Kept literal here rather than imported: these hook
# scripts are launched standalone by path, and an ImportError would take the
# gate out entirely.
OK = "ok"
NO_REPO = "no-repo"
UNDET = "undetermined"

_NO_REPO_MARKERS = ("not a git repository", "not a git repo")


def _probe(cwd: str, *args: str) -> tuple[str, str | None]:
    """Run a git query and classify the outcome as OK / NO_REPO / UNDET."""
    try:
        r = subprocess.run(
            ("git", *args), cwd=cwd, capture_output=True, text=True, timeout=10
        )
    except (OSError, subprocess.SubprocessError):
        return UNDET, None  # git not runnable here, or it timed out
    if r.returncode == 0:
        value = r.stdout.strip()
        return (OK, value) if value else (UNDET, None)
    if any(m in (r.stderr or "").lower() for m in _NO_REPO_MARKERS):
        return NO_REPO, None
    return UNDET, None


def _main_root() -> tuple[str, str | None]:
    """(state, main-tree root).

    OK       -> guard mutations landing under the returned root.
    NO_REPO  -> git gave a determinate "nothing to guard here" (not a
                repository at all, or this anchor IS a linked worktree, which is
                the sanctioned place to work). Allow.
    UNDET    -> could not tell. Resolves to deny at the callsite (3.).
    """
    proj = os.environ.get("CLAUDE_PROJECT_DIR")
    if not proj:
        # No project anchor. Fall back to the intrinsic test from this process's
        # own cwd rather than allowing everything — the twin
        # guard-maintree-edit.py makes the same fallback for the same reason.
        proj = os.getcwd()
    proj = os.path.realpath(proj)
    # Confirm it is a main checkout (git-dir == common-dir); if it is itself a
    # worktree or not a repo, we have no main tree to guard from here.
    st_gd, gd = _probe(proj, "rev-parse", "--absolute-git-dir")
    st_cm, cm = _probe(proj, "rev-parse", "--path-format=absolute", "--git-common-dir")
    # Undetermined first: a NO_REPO reading of one probe proves nothing while the
    # other one could not answer at all.
    if st_gd == UNDET or st_cm == UNDET:
        return UNDET, None
    if st_gd == NO_REPO or st_cm == NO_REPO:
        return NO_REPO, None
    assert gd is not None and cm is not None  # OK implies a value
    if os.path.realpath(gd) != os.path.realpath(cm):
        return NO_REPO, None  # a linked worktree — mutating here is the point
    return OK, proj


def _under(child: str, parent: str) -> bool:
    parent = parent.rstrip("/")
    return child == parent or child.startswith(parent + "/")


def _resolve(root: str, path: str) -> str:
    if not os.path.isabs(path):
        path = os.path.join(root, path)
    return os.path.realpath(path)


# Constructs this process cannot expand from the command string alone. `~` is
# NOT here: it expands deterministically, so it is expanded and then judged.
_UNRESOLVABLE = set("$`*?{}")

# Only these two variables are expanded, and only as whole words: they are the
# ones whose value this process actually knows. `$HOMEBREW` must not become
# `<home>BREW`.
_KNOWN_VAR = re.compile(r"\$\{(HOME|PWD)\}|\$(HOME|PWD)(?![A-Za-z0-9_])")


def _expand(path: str) -> str:
    """Expand the deterministic forms only: a leading `~`, $HOME and $PWD."""
    home = os.environ.get("HOME")
    if home and (path == "~" or path.startswith("~/")):
        path = home + path[1:]

    def sub(m: re.Match) -> str:
        name = m.group(1) or m.group(2)
        val = os.environ.get(name)
        return val if val else m.group(0)  # unknown value stays unresolvable

    return _KNOWN_VAR.sub(sub, path)


def _literal_prefix(path: str) -> str:
    """The longest leading path segment sequence free of unresolvable syntax.

    `/a/b/*.rs` -> `/a/b/`; `*.rs` -> `` (which resolves to the root).
    """
    hits = [path.find(c) for c in _UNRESOLVABLE if c in path]
    if not hits:
        return path
    cut = path.rfind("/", 0, min(hits))
    return path[: cut + 1] if cut >= 0 else ""


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


def _hits_main(
    root: str, path: str | None, own_gitdir: str | None, anchor: str | None = None
) -> bool:
    """True if `path` lands on a non-ignored location under the main tree.

    Relative paths resolve against `anchor` (the directory the command has
    `cd`-ed into; the main root by default). `path` None — an interpreter that
    writes through a path it computes — and a relative path whose anchor could
    not be resolved are both "cannot determine": they count as hitting main
    unless the anchor is provably outside it (CLAUDE.md 3)."""
    if anchor is None and (path is None or not os.path.isabs(_expand(path))):
        return True
    base = anchor if anchor is not None else root
    if path is None:
        return _under(base, root)
    if base != root and not os.path.isabs(_expand(path)):
        # Re-anchored: judge the anchor-resolved absolute path below.
        path = os.path.join(base, _expand(path))
    path = _expand(path)
    if any(c in path for c in _UNRESOLVABLE):
        # A shell variable this process does not know ($WT), a glob, or a brace
        # expansion. It cannot be resolved to a literal path — but "unresolvable"
        # is not "harmless" (3.), so decide on the longest LITERAL prefix: the
        # expansion can only land under the main tree if that prefix and the root
        # lie on the same ancestor chain. `<wt>/*.rs` provably cannot (disjoint
        # branches) and is allowed; `<parent-of-root>/*` and a bare `*.rs` both
        # can, and are refused.
        anchor = _resolve(root, _literal_prefix(path))
        return _under(anchor, root) or _under(root, anchor)
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
    # exit 0 = ignored (local scratch, allowed). Anything else — including git
    # being unrunnable — means we did not establish that it is ignored, so it
    # counts as hitting main (3.).
    try:
        ignored = subprocess.run(
            ("git", "check-ignore", "-q", resolved),
            cwd=root, capture_output=True, timeout=10,
        )
    except (OSError, subprocess.SubprocessError):
        return True
    return ignored.returncode != 0


# `<<WORD`, `<<'WORD'`, `<<"WORD"`, `<<-WORD`. Not `<<<` (a here-STRING has no
# body): after `<<` the next char there is `<`, which matches neither branch.
_HEREDOC_START = re.compile(
    r"<<-?\s*(?:'([^']*)'|\"([^\"]*)\"|([A-Za-z_][A-Za-z0-9_]*))"
)


def _strip_heredoc_bodies(command: str) -> str:
    """Drop here-document BODIES so an apostrophe in prose cannot make the whole
    command untokenizable. The redirection targets that matter all appear on the
    line that OPENS the heredoc, which is kept.

    Fail-safe on ambiguity: a `<<` that appears inside a quoted string has no
    real terminator, and swallowing lines until EOF could hide a later mutation.
    So lines are dropped only when the terminator is actually found; otherwise
    the text is left exactly as-is and the tokenizer's verdict (deny) stands.
    """
    lines = command.split("\n")
    out: list[str] = []
    i = 0
    while i < len(lines):
        line = lines[i]
        out.append(line)
        i += 1
        for m in _HEREDOC_START.finditer(line):
            delim = m.group(1) or m.group(2) or m.group(3)
            end = i
            while end < len(lines) and lines[end].strip() != delim:
                end += 1
            if end < len(lines):
                i = end + 1  # body + terminator consumed
            # else: no terminator in sight — keep the lines, decide on the text.
    return "\n".join(out)


_WORD_BREAK = set(" \t\n;&|()<>")


def _newlines_to_separators(command: str) -> str:
    """Rewrite every UNQUOTED newline as an explicit ` ; ` command separator, and
    drop comments, so each line becomes its own segment.

    A newline ends a simple command exactly like `;` does, but the tokenizer
    used to turn it into an empty token that no separator check recognised: the
    lines of `echo hi<NL>rm <main>/f` merged into ONE segment whose program was
    `echo`, so the second-line `rm` was never judged (9fdb49d8). The reverse
    also held — a second line's operands became extra operands of the first
    line's `cp`, refusing a harmless command.

    Only newlines that the shell itself treats as separators are rewritten:
      * inside '…' or "…" a newline is data and is kept;
      * backslash-newline (outside '…') is a line CONTINUATION and is removed,
        joining the two lines, as the shell does;
      * a `#` at the start of a word begins a comment that runs to the newline;
        the comment is dropped here (and the tokenizer's own, looser comment
        rule is switched off), because a comment swallowing a rewritten `;`
        would hide every later line.
    Here-document bodies are removed before this runs (_strip_heredoc_bodies).
    An unbalanced quote leaves the rest of the text untouched, so the tokenizer
    still fails on it and the command is refused (3.).
    """
    out: list[str] = []
    i, n = 0, len(command)
    quote: str | None = None
    while i < n:
        c = command[i]
        if quote == "'":
            out.append(c)
            if c == "'":
                quote = None
            i += 1
            continue
        if c == "\\" and i + 1 < n:
            if command[i + 1] == "\n":
                i += 2  # line continuation: not a separator, join the lines
                continue
            out.append(command[i : i + 2])
            i += 2
            continue
        if quote == '"':
            out.append(c)
            if c == '"':
                quote = None
            i += 1
            continue
        if c in ("'", '"'):
            quote = c
            out.append(c)
        elif c == "#" and (not out or out[-1][-1:] in _WORD_BREAK):
            while i < n and command[i] != "\n":
                i += 1
            continue  # the newline (if any) is handled on the next pass
        elif c == "\n":
            out.append(" ; ")
        else:
            out.append(c)
        i += 1
    return "".join(out)


def _tokenize(command: str) -> list[str] | None:
    lexer = shlex.shlex(
        _newlines_to_separators(command), posix=True, punctuation_chars="();<>|&"
    )
    lexer.whitespace_split = True
    # Comments were already removed with shell word-start semantics; shlex's own
    # rule also fires mid-word (`a#b`) and would swallow the rest of the input.
    lexer.commenters = ""
    try:
        return list(lexer)
    except ValueError:
        return None


# A target is (path, anchor): `path` None means "writes somewhere this process
# cannot name" (an interpreter that writes through a computed path); `anchor` is
# the directory relative paths resolve against, None when a `cd` went somewhere
# this process cannot resolve (`cd -`, `cd $X`).

# Shells whose `-c <string>` payload is itself shell, judged by the same rules.
SHELLS = {"sh", "bash", "zsh", "dash", "ksh"}
# Prefix commands that run their argv as the real command (`env X=1 rm f`).
PASSTHROUGH = {"env", "nohup", "time", "nice", "command", "exec", "sudo", "stdbuf"}
AWKS = {"awk", "gawk", "mawk", "nawk"}

# Inline interpreter code that may write, delete, or spawn a shell (ae4543d5).
# A read-only one-liner (`open(p)`, `json.load`) matches none of these, so the
# common "inspect a file on main" form stays allowed. A match makes every
# path-like string literal (and positional operand) a write target; a match with
# no nameable path is "cannot determine", which resolves to deny when it runs
# against the main tree (CLAUDE.md 3).
_WRITE_MARKER = re.compile(
    r"""open\s*\([^)]*['"][rbt]*[wax+][bt+]*['"]"""  # python open(p, 'w'/'a'/'x'/'r+')
    r"""|open\s*\(?[^;]*['"]\s*\+?[>|]"""  # perl open(F, '>f') / ">>f"
    r"""|write_text|write_bytes|\.touch\(|\bunlink|\brmdir|\brmtree|\bshutil\."""
    r"""|\bos\.(?:remove|rename|replace|makedirs|mkdir|truncate|system|popen|exec)"""
    r"""|\bsubprocess\b|\bsystem\s*\(|\bexec[lv]?p?\s*\("""
    r"""|\b(?:writeFile|appendFile|rm|rename|mkdir|copyFile|cp|truncate|symlink|chmod)(?:Sync)?\s*\("""
    r"""|createWriteStream|\bFileUtils\b|\bFile\.(?:write|open|delete|rename)"""
    r"""|\brename\s*\(|\bprintf?\b[^;]*>|`""",
)
_STR_LIT = re.compile(r"""'([^']*)'|"([^"]*)\"""")
_PATHISH = re.compile(r"^[\w.~$/{}@+:-]+$")


def _code_targets(code: str, operands: list[str], anchor: str | None) -> list:
    """Targets of an inline interpreter program: none unless it can write."""
    if not _WRITE_MARKER.search(code):
        return []
    lits = [m.group(1) if m.group(1) is not None else m.group(2) for m in _STR_LIT.finditer(code)]
    paths = [s for s in lits if s and _PATHISH.match(s) and ("/" in s or "." in s.strip("."))]
    found = [(p, anchor) for p in paths + operands]
    # Writes, but through nothing this process can name: cannot determine.
    return found or [(None, anchor)]


def _interp_targets(prog: str, rest: list[str], anchor: str | None) -> list:
    """python -c / node -e / ruby -e / perl -e / awk 'prog' (ae4543d5)."""
    codes: list[str] = []
    operands: list[str] = []
    targets: list = []
    in_place = False
    i = 0
    if prog.startswith("python"):
        code_flag = lambda a: a.startswith("-") and not a.startswith("--") and a.endswith("c")  # noqa: E731
    elif prog in ("node", "nodejs"):
        code_flag = lambda a: a in ("-e", "--eval", "-p", "--print")  # noqa: E731
    elif prog in AWKS:
        code_flag = None
    else:  # perl, ruby: -e / -E, also bundled (-pe, -ne, -pie)
        code_flag = lambda a: a.startswith("-") and not a.startswith("--") and a[-1:] in ("e", "E")  # noqa: E731
    while i < len(rest):
        a = rest[i]
        if prog in AWKS:
            if a in ("-v", "-F") and i + 1 < len(rest):
                i += 2
                continue
            if a.startswith("-f"):
                return targets  # program from a file: not visible here (known hole)
            if not a.startswith("-") and not codes:
                codes.append(a)
            elif not a.startswith("-"):
                operands.append(a)
            i += 1
            continue
        if prog in ("perl", "ruby") and a.startswith("-") and not a.startswith("--") and "i" in a[1:].split(".")[0]:
            in_place = True  # -i, -pi, -pie, -i.bak — the flag-bundle scan
        if code_flag(a) and i + 1 < len(rest):
            codes.append(rest[i + 1])
            i += 2
            if prog.startswith("python"):
                operands += rest[i:]  # everything after `-c code` is sys.argv
                break
            continue
        if not a.startswith("-"):
            operands.append(a)
        i += 1
    if in_place:
        targets += [(p, anchor) for p in operands]
    for code in codes:
        targets += _code_targets(code, [] if in_place else operands, anchor)
    return targets


def _collect(tokens: list[str], root: str, anchor: str | None, depth: int = 0):
    """Filesystem targets this command would mutate (best effort), each paired
    with the directory it resolves against. None = cannot determine (deny).

    `cd <dir>` moves the anchor for the segments AFTER it (and a `( … )`
    subshell restores it), so `cd <wt> && sed -i … f` resolves `f` inside the
    worktree, while `cd /tmp && echo > <main>/p` still lands on main — the old
    rule allowed the WHOLE command once any `cd`/`-C` left the main tree.
    """
    if depth > 8:
        return None  # nesting this deep is not something to guess about
    targets: list = []
    stack: list[str | None] = []
    seg: list[str] = []

    def flush(seg: list[str]):
        nonlocal anchor
        argv = list(seg)
        while True:
            k = 0
            while k < len(argv) and "=" in argv[k] and not argv[k].startswith("-"):
                k += 1
            argv = argv[k:]
            if not argv:
                return []
            prog = argv[0].split("/")[-1]
            if prog not in PASSTHROUGH:
                break
            argv = argv[1:]
            while argv and argv[0].startswith("-"):
                flag = argv.pop(0)
                if flag in ("-n", "-u", "-C", "-g", "-o", "-e") and argv:
                    argv.pop(0)  # nice -n N / sudo -u U / env -u VAR / stdbuf -o L
        rest = argv[1:]
        out: list = []
        if prog in ("cd", "pushd"):
            ops = [a for a in rest if not a.startswith("-")]
            if not ops:
                anchor = os.environ.get("HOME") or None
            else:
                dest = _expand(ops[0])
                if any(c in dest for c in _UNRESOLVABLE) or (
                    anchor is None and not os.path.isabs(dest)
                ):
                    anchor = None
                else:
                    anchor = _resolve(anchor, dest)
            return out
        if prog in SHELLS:
            for j, a in enumerate(rest):
                if not a.startswith("-"):
                    break  # a script file / stdin: not visible here (known hole)
                if not a.startswith("--") and "c" in a[1:]:
                    if j + 1 >= len(rest):
                        return None
                    inner = _tokenize(_strip_heredoc_bodies(rest[j + 1]))
                    if inner is None:
                        return None
                    return _collect(inner, root, anchor, depth + 1)
            return out
        if prog == "eval":
            inner = _tokenize(_strip_heredoc_bodies(" ".join(rest)))
            if inner is None:
                return None
            return _collect(inner, root, anchor, depth + 1)
        if prog.startswith("python") or prog in ("node", "nodejs", "perl", "ruby") or prog in AWKS:
            return _interp_targets(prog, rest, anchor)
        if prog in TARGET_ALL:
            out += [(a, anchor) for a in rest if not a.startswith("-")]
        elif prog in ("sed", "gsed") and any(
            a == "-i" or a.startswith("-i") or a.startswith("--in-place")
            or re.match(r"^-[nErsuz]+i", a)
            for a in rest
        ):
            ops = [a for a in rest if not a.startswith("-")]
            # `sed -i 's/a/b/' f1 f2` — the FIRST operand is the script, not a
            # file, unless the script came via -e/-f (then all operands are
            # files). Treating the script as a path resolved it under the main
            # root and wrongly refused a legitimate worktree sed.
            has_expr = any(
                a in ("-e", "-f") or a.startswith("-e") or a.startswith("-f")
                or a.startswith("--expression") or a.startswith("--file")
                for a in rest
            )
            out += [(a, anchor) for a in (ops if has_expr else ops[1:])]
        elif prog == "dd":
            out += [(a[3:], anchor) for a in rest if a.startswith("of=")]
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
        return out

    i = 0
    while i < len(tokens):
        tok = tokens[i]
        if tok in REDIR or REDIR_NUM.match(tok):
            if i + 1 < len(tokens):
                targets.append((tokens[i + 1], anchor))
            i += 2
            continue
        if tok in SEPARATORS:
            got = flush(seg) if seg else []
            if got is None:
                return None
            targets += got
            seg = []
            if tok == "(":
                stack.append(anchor)
            elif tok == ")" and stack:
                anchor = stack.pop()
            i += 1
            continue
        seg.append(tok)
        i += 1
    got = flush(seg) if seg else []
    if got is None:
        return None
    return targets + got


DENY = """Refused: `{cmd}` mutates this project's MAIN working tree.

CLAUDE.md 最上位の方針 8: nothing edits, adds to, or deletes from the main tree
directly — every mutation happens in a `git worktree` and reaches main through a
merge. Another session is always assumed to be sharing this index.

    git worktree add -b <branch> <path> <base>
    # run the mutation against files inside that worktree, commit, then merge.
"""

DENY_UNPARSEABLE = """Refused: could not determine what `{cmd}` writes to.

The command does not tokenize (usually an unbalanced quote), so this gate cannot
tell whether it mutates the MAIN working tree. "Could not determine" is not
"does not touch main" (CLAUDE.md 最上位の方針 3), so it resolves to a refusal
rather than an allow.

Rewrite it so it parses — balance the quotes, or put the text in a here-document
whose body this gate skips — and run it again. If it does mutate the main tree,
do it in a worktree instead.
"""

DENY_NO_ROOT = """Refused: could not determine this project's main working tree.

git could not answer where the main checkout is (it was not runnable, timed out,
or failed for a reason other than "not a git repository"), so no target in this
command could be judged against it. A check that could not run has not passed
(CLAUDE.md 最上位の方針 3), so this resolves to a refusal.

Fix whatever is stopping git from answering — or set CLAUDE_PROJECT_DIR to the
main checkout — and run it again.
"""


def _first_line(command: str) -> str:
    stripped = command.strip().splitlines()
    return stripped[0][:120] if stripped else ""


def decide(payload: dict) -> tuple[int, str]:
    if payload.get("tool_name") != "Bash":
        return 0, ""
    tool_input = payload.get("tool_input")
    if not isinstance(tool_input, dict):
        return 2, DENY_BAD_PAYLOAD.format(why="Bash call without a tool_input object")
    command = tool_input.get("command")
    if not isinstance(command, str):
        return 2, DENY_BAD_PAYLOAD.format(why="Bash call without a string command")
    if not command.strip():
        return 0, ""  # an empty command runs nothing, so it mutates nothing

    state, root = _main_root()
    if state == UNDET:
        # Could not establish whether there is a main tree here at all, so every
        # target below would be judged against nothing. A check that could not
        # run has not passed (3.).
        return 2, DENY_NO_ROOT
    if state != OK:
        # git gave a determinate answer: not a repository, or this anchor is a
        # linked worktree. Either way there is no main tree to protect.
        return 0, ""
    assert root is not None

    tokens = _tokenize(_strip_heredoc_bodies(command))
    if tokens is None:
        # The command does not tokenize, so its filesystem targets are unknown —
        # which is not the same as "it has none". This used to allow, and a
        # here-doc body containing an apostrophe was enough to walk a write to
        # main straight past the gate.
        return 2, DENY_UNPARSEABLE.format(cmd=_first_line(command))

    targets = _collect(tokens, root, root)
    if targets is None:
        # A nested `sh -c` / `eval` payload that does not tokenize: its targets
        # are unknown, which is not "it has none" (3.).
        return 2, DENY_UNPARSEABLE.format(cmd=_first_line(command))

    own_gitdir = _own_worktree_gitdir(root)
    for target, anchor in targets:
        if _hits_main(root, target, own_gitdir, anchor):
            shown = target if target is not None else "<a path the inline program computes>"
            return 2, DENY.format(cmd=f"{_first_line(command)}` (target `{shown}")
    return 0, ""


DENY_BAD_PAYLOAD = """Refused: could not read the hook payload ({why}).

This gate received no usable JSON object describing the tool call, so it cannot
tell which command is about to run or whether it mutates the MAIN working tree.
A payload this gate could not read is not a command it checked (CLAUDE.md
最上位の方針 3), so it resolves to a refusal rather than an allow.
"""


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError) as e:
        sys.stderr.write(DENY_BAD_PAYLOAD.format(why=f"not JSON: {e}"))
        return 2
    if not isinstance(payload, dict):
        sys.stderr.write(
            DENY_BAD_PAYLOAD.format(
                why=f"JSON {type(payload).__name__}, not an object"
            )
        )
        return 2
    code, reason = decide(payload)
    if code != 0:
        sys.stderr.write(reason)
    return code


if __name__ == "__main__":
    sys.exit(main())

# KNOWN, UNCLOSED HOLES (recorded, not hidden — CLAUDE.md 4):
#   * programs this process cannot see: a script FILE (`bash x.sh`,
#     `python3 x.py`, `awk -f p`), an interpreter reading its program from stdin
#     or a here-document (`python3 - <<EOF`; the body is stripped as data), and
#     `xargs` (its operands arrive on stdin).
#   * inline interpreter code is matched on a write-marker regex, not parsed: a
#     write spelled in a way the regex does not know is allowed.
#   * moving/copying INTO the main tree from an outside source is caught (dest is
#     scanned) but a tool that resolves its own paths (a Makefile, cargo) is not.
#   All of these are caught at the durable moment by check-worktree-isolation.py,
#   which refuses the commit regardless of how the tree was dirtied.
