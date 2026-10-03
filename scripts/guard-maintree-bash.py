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

JUDGED BY EFFECT, NOT BY argv[0] (ae4543d5). The same write used to pass by
changing its spelling; each of these is now followed to the path it writes:
  * shell wrappers: `sh -c`, `bash -lc` (any bundle containing `c`), `eval`,
    `env …`, `nohup`, `time`, `nice`, `sudo`, `timeout`, `xargs`, `command`,
    `exec`, `find -exec`, a quoted `"$( … )"` or backquote — the payload is
    tokenized and judged by this same procedure, recursively (depth-capped);
  * interpreters whose payload is NOT shell — `python -c` / `python - <<EOF`,
    `node -e`, `perl -e`, `ruby -e`, an awk program: if the payload contains a
    write primitive (open(…,'w'), write_text, writeFileSync, `print > "f"`,
    os.system, …) then every literal path in it — and every operand handed to
    it — is a write-target candidate. A payload that writes but names no
    literal path at all is REFUSED (cannot determine). A literal that begins
    with a trailing run of the main root's components (`'/src/harness/x'`,
    the shape of `$HOME + '/src/harness/x'`) counts as main;
  * in-place flags in any spelling: `-i`, `-i.bak`, `-pi`, `-Ei`,
    `--in-place[=sfx]`, BSD `-i ''`, gawk `-i inplace`;
  * downloaders and extractors: `curl -o/--output/-O/--output-dir`,
    `wget -O/-P` (and plain `wget`, which writes into the cwd), `tar -x`
    (`-C`/`--directory` or the cwd), `tar -c` (the archive), `unzip` (`-d` or
    the cwd), `find -delete`, `chmod/chown/chgrp`, `rsync/scp/ditto`.

CWD AND VARIABLES ARE TRACKED THROUGH THE COMMAND, not used as a blanket
excuse. `cd <dir>` changes what a RELATIVE path resolves against for the
commands after it; an ABSOLUTE path into main is a hit no matter what was
`cd`-ed to before it, and `git -C <dir>` re-anchors only that one git command.
Relative paths start out resolving against the main root. A `cd` inside a
subshell `( … )` / `$( … )`, a pipeline stage or a backgrounded job does not
change the cwd seen afterwards. A `cd` whose success is uncertain — one that
ran conditionally (after `&&`/`||`, or before `||`), one inside
if/while/for/case, or one to a directory that does not exist when it is not
followed by `&&` — holds only where it certainly ran (the rest of its `&&`
chain / its branch); after that the cwd is the MERGE of both possibilities,
which is UNKNOWN wherever they differ, and a relative write against an unknown
cwd is refused.

Variables are handled the same way. `S=<dir>`, `export S=<dir>`, `declare`,
`local`, `readonly` and `unset` earlier in the SAME command line are tracked
and expanded (`$S`, `${S}`, `"$S"`), with the same uncertainty rules as `cd`.
A value only known at runtime (`S=$(…)`, `read S`, a loop variable, two
branches that disagree) becomes unresolvable. A `S=x cmd` PREFIX assignment
only reaches cmd's environment and does not expand `$S` in its own arguments.
What is chosen for a variable that is NOT assigned in the command and is
therefore inherited from the session's shell: only `$HOME` (and `~`) is taken
from this hook's environment, because it is the one value the hook reliably
shares with the session. `$PWD` is the TRACKED cwd (initially this hook's own
cwd). Every other inherited variable is treated as unknown — this hook's
environment is not the Bash tool's shell, so reading it would judge a value
that may not be the one used. An unknown variable is refused only if it could
point into main: the path is judged on its longest LITERAL prefix, so
`$X/f`, `~user/f` and `<parent-of-main>/$X` are refused, while
`/tmp/$X/f` and `<worktree>/$X` provably cannot reach main and are allowed.

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

# Stand-in values. Each contains `$`, so a path built from one is unresolvable
# and is judged on its literal prefix (see _hit) — never silently resolved.
UNKNOWN_VAL = "$__undetermined__"  # a variable whose value could not be tracked
SUBST_WORD = "$__substitution__"   # the output of `$( … )` / `<( … )`
TILDE_USER = "$__tilde_user__"     # `~user/…`, which this process does not resolve

_VAR_REF = re.compile(r"\$\{([A-Za-z_][A-Za-z0-9_]*)\}|\$([A-Za-z_][A-Za-z0-9_]*)")
_ASSIGN = re.compile(r"^([A-Za-z_][A-Za-z0-9_]*)\+?=(.*)$", re.S)


class _State:
    """What the shell's state is known to be at a point in the command.

    rel   directory a RELATIVE path resolves against; None = unknown.
    pwd   value of $PWD; None = unknown.
    vars  shell variables assigned earlier in this same command, already
          expanded. A key whose value is UNKNOWN_VAL was assigned something this
          process cannot know (`$(…)`, `read`, a loop variable, two branches
          that disagree). A key that is ABSENT was never assigned here: its value
          is whatever the session's shell inherited, which this process cannot
          see — the hook's own environment is NOT the Bash tool's shell.
    """

    __slots__ = ("rel", "pwd", "vars")

    def __init__(self, rel: str | None, pwd: str | None, vars_: dict[str, str]):
        self.rel = rel
        self.pwd = pwd
        self.vars = vars_

    def copy(self) -> "_State":
        return _State(self.rel, self.pwd, dict(self.vars))


def _merge(a: _State, b: _State) -> _State:
    """The state when either `a` or `b` may hold: whatever they disagree on
    becomes unknown (never the more permissive of the two)."""
    vs: dict[str, str] = {}
    for k in set(a.vars) | set(b.vars):
        va, vb = a.vars.get(k), b.vars.get(k)
        vs[k] = va if (va is not None and va == vb) else UNKNOWN_VAL
    return _State(
        a.rel if a.rel == b.rel else None,
        a.pwd if a.pwd == b.pwd else None,
        vs,
    )


def _expand(path: str, st: _State) -> str:
    """Expand what this process can know: a leading `~`, variables assigned
    earlier in the same command, `$HOME`, and `$PWD` (the tracked cwd).
    Anything else is left in place, and so stays unresolvable."""
    if path.startswith("~"):
        if path == "~" or path.startswith("~/"):
            home = st.vars.get("HOME", os.environ.get("HOME"))
            path = (home + path[1:]) if home else TILDE_USER + path[1:]
        else:
            path = TILDE_USER + path[1:]

    def sub(m: re.Match) -> str:
        name = m.group(1) or m.group(2)
        if name in st.vars:
            return st.vars[name]
        if name == "HOME":
            return os.environ.get("HOME") or m.group(0)
        if name == "PWD":
            return st.pwd or m.group(0)
        return m.group(0)

    # One pass: a substituted value is never itself re-expanded.
    return _VAR_REF.sub(sub, path)


def _literal_prefix(path: str) -> str:
    """The longest leading path segment sequence free of unresolvable syntax.

    `/a/b/*.rs` -> `/a/b/`; `*.rs` -> `` (which resolves to the cwd).
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


def _hits_main(root: str, path: str, own_gitdir: str | None, st: _State) -> bool:
    """True if `path` (as written at a point where the shell state is `st`)
    could land on a non-ignored location under the main tree."""
    path = _expand(path, st)
    if any(c in path for c in _UNRESOLVABLE):
        # A shell variable this process does not know ($WT), a glob, or a brace
        # expansion. It cannot be resolved to a literal path — but "unresolvable"
        # is not "harmless" (3.), so decide on the longest LITERAL prefix: the
        # expansion can only land under the main tree if that prefix and the root
        # lie on the same ancestor chain. `<wt>/*.rs` provably cannot (disjoint
        # branches) and is allowed; `<parent-of-root>/*` and a bare `*.rs` both
        # can, and are refused.
        prefix = _literal_prefix(path)
        if not os.path.isabs(prefix):
            if st.rel is None:
                return True  # relative to a cwd we could not determine
            anchor = _resolve(st.rel, prefix)
        else:
            anchor = os.path.realpath(prefix)
        return _under(anchor, root) or _under(root, anchor)
    if not os.path.isabs(path) and st.rel is None:
        return True  # relative to a cwd we could not determine (3.)
    resolved = _resolve(st.rel or root, path)
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


def _fragment_of_root(root: str, path: str) -> bool:
    """True if `path` BEGINS with a trailing run of the main root's components,
    e.g. root `/Users/u/src/harness` and path `/src/harness/x` or
    `harness/x`. That is the shape of a main-tree path rebuilt at runtime from
    a value this process cannot see (`os.environ['HOME'] + '/src/harness/x'`).
    Applied to interpreter payload literals only."""
    rc = [c for c in root.split("/") if c]
    pc = [c for c in os.path.normpath(path).split("/") if c and c != "."]
    for k in range(1, len(rc) + 1):
        if pc[:k] == rc[-k:]:
            return True
    return False


# `<<WORD`, `<<'WORD'`, `<<"WORD"`, `<<-WORD`. Not `<<<` (a here-STRING has no
# body): the look-arounds keep the regex off every `<<` inside a `<<<`.
_HEREDOC_START = re.compile(
    r"(?<!<)<<(?!<)-?\s*(?:'([^']*)'|\"([^\"]*)\"|([A-Za-z_][A-Za-z0-9_]*))"
)


def _strip_heredoc_bodies(command: str) -> tuple[str, list[str | None]]:
    """Drop here-document BODIES so an apostrophe in prose cannot make the whole
    command untokenizable. The redirection targets that matter all appear on the
    line that OPENS the heredoc, which is kept.

    Returns the stripped text and the bodies in order of appearance (None for a
    heredoc whose terminator was not found). The bodies are kept because a
    heredoc fed to an interpreter (`python3 - <<EOF`, `bash <<EOF`) IS code, and
    is judged as such.

    Fail-safe on ambiguity: a `<<` that appears inside a quoted string has no
    real terminator, and swallowing lines until EOF could hide a later mutation.
    So lines are dropped only when the terminator is actually found; otherwise
    the text is left exactly as-is and the tokenizer's verdict (deny) stands.
    """
    lines = command.split("\n")
    out: list[str] = []
    bodies: list[str | None] = []
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
                bodies.append("\n".join(lines[i:end]))
                i = end + 1  # body + terminator consumed
            else:
                bodies.append(None)
                # no terminator in sight — keep the lines, decide on the text.
    return "\n".join(out), bodies


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


_PUNCT = set("();<>|&")
# Longest first. shlex returns a RUN of punctuation as one token (`);`, `>&`),
# so runs are re-split into the operators the shell would see.
_OPERATORS = sorted(
    ["&>>", "<<<", ";;&", "&&", "||", ";;", ";&", "|&", ">>", ">|", ">&", "&>",
     "<<", "<&", "<>", "<(", ">(", ";", "&", "|", "(", ")", "<", ">"],
    key=len, reverse=True,
)


def _split_punct(tok: str) -> list[str]:
    if not tok or any(c not in _PUNCT for c in tok):
        return [tok]
    out: list[str] = []
    i = 0
    while i < len(tok):
        for op in _OPERATORS:
            if tok.startswith(op, i):
                out.append(op)
                i += len(op)
                break
        else:  # unreachable: every char of _PUNCT is a 1-char operator
            out.append(tok[i])
            i += 1
    return out


def _tokenize(command: str) -> list[str] | None:
    lexer = shlex.shlex(
        _newlines_to_separators(command), posix=True, punctuation_chars="();<>|&"
    )
    lexer.whitespace_split = True
    # Comments were already removed with shell word-start semantics; shlex's own
    # rule also fires mid-word (`a#b`) and would swallow the rest of the input.
    lexer.commenters = ""
    try:
        raw = list(lexer)
    except ValueError:
        return None
    return [t for tok in raw for t in _split_punct(tok)]


# ---------------------------------------------------------------------------
# Effect analysis
# ---------------------------------------------------------------------------

class _Hit(Exception):
    """A write target that lands on the main tree."""


class _Undet(Exception):
    """Could not determine what a (nested) command writes to — resolves to deny."""


class _Unparseable(Exception):
    """The top-level command does not tokenize."""


MAX_DEPTH = 6

REDIR_OUT = {">", ">>", ">|", "&>", "&>>", "<>"}
SEPARATORS = frozenset(
    {";", "&&", "||", "|", "&", ";;", ";&", ";;&", "|&", "(", ")", "<(", ">("}
)
_CHAIN_END = {";", ";;", ";&", ";;&", "&", None}

# Commands where every non-flag operand is a filesystem target that gets created,
# overwritten, or removed. Over-inclusive on purpose (a refusal is cheap).
TARGET_ALL = {
    "rm", "unlink", "rmdir", "mv", "cp", "tee", "touch", "mkdir",
    "ln", "install", "truncate", "shred",
}
# The first operand is a mode/owner, the rest are targets.
TARGET_AFTER_FIRST = {"chmod", "chown", "chgrp"}
# The LAST operand is the destination.
TARGET_LAST = {"rsync", "scp", "ditto"}
# Programs whose targets `xargs` would feed from stdin.
STDIN_MUTATORS = TARGET_ALL | TARGET_AFTER_FIRST | {"sed", "gsed", "perl", "ruby"}

SHELLS = {"sh", "bash", "zsh", "dash", "ksh", "mksh", "fish", "busybox"}
AWKS = {"awk", "gawk", "nawk", "mawk"}
_PYTHON = re.compile(r"^(?:python[0-9.]*|pypy[0-9.]*)$")

# Words that precede a command without being it.
LEAD_KEYWORDS = {"!", "{", "}", "if", "then", "else", "elif", "while", "until",
                 "do", "fi", "done", "esac", "coproc"}
BLOCK_OPEN = {"if", "while", "until", "for", "select", "case"}
BLOCK_CLOSE = {"fi", "done", "esac"}

# Wrappers that run their operand as a command: {name: options taking an arg}.
WRAPPERS: dict[str, set[str]] = {
    "nohup": set(), "command": {"-p"}, "builtin": set(), "exec": {"-a"},
    "nice": {"-n"}, "sudo": {"-u", "-g", "-h", "-p", "-C", "-r", "-t", "-U", "-T", "-D"},
    "doas": {"-u", "-C"}, "timeout": {"-s", "-k"}, "gtimeout": {"-s", "-k"},
    "stdbuf": {"-i", "-o", "-e"}, "caffeinate": {"-w", "-t"}, "chronic": set(),
    "unbuffer": set(), "ionice": {"-c", "-n", "-p"}, "time": {"-f", "-o"},
    "xargs": {"-I", "-n", "-P", "-L", "-d", "-E", "-s", "-a"},
}
# Wrappers that run the command IN the current shell (state changes persist).
SAME_SHELL_WRAPPERS = {"command", "builtin", "exec", "time"}

# Write primitives per interpreter. Over-inclusive on purpose: a match only
# means "this payload may write", and its literal paths are then judged.
_PY_MODE = re.compile(r"""['"](?:[rbtU]*[wax][rbt+]*|[rbt]*\+[rbt]*)['"]""")
_WRITE = {
    "python": re.compile(
        r"\.write_(?:text|bytes)\s*\(|\.(?:touch|mkdir|unlink|rmdir|rename|symlink_to|"
        r"hardlink_to|chmod|extractall|extract)\s*\(|\b(?:os|shutil)\.(?:remove|unlink|"
        r"rename|replace|renames|makedirs|mkdir|rmdir|removedirs|symlink|link|truncate|"
        r"chmod|system|popen|exec\w*|spawn\w*|copy\w*|move|rmtree|make_archive|"
        r"unpack_archive)\b|\bsubprocess\b|\bpty\b|\bsqlite3\.connect\b|"
        r"\binplace\s*=\s*(?:True|1)\b"
    ),
    "node": re.compile(
        r"\b(?:writeFile|appendFile|createWriteStream|mkdir|mkdtemp|rmdir|rm|unlink|"
        r"rename|copyFile|cp|symlink|link|truncate|chmod|utimes|open)(?:Sync)?\s*\(|"
        r"\bchild_process\b|\b(?:exec|execFile|spawn)(?:Sync)?\s*\(|"
        r"\bBun\.write\b|\bDeno\.\w+"
    ),
    "perl": re.compile(
        r"""\bopen\b[^;]*?['"]\s*(?:\+?>|\+<|\|)|['"]\s*(?:>>?|\+<)\s*['"]|"""
        r"\b(?:unlink|rename|mkdir|rmdir|symlink|link|truncate|chmod|system|exec|utime|"
        r"sysopen|qx|copy|move|make_path|mkpath|remove_tree|rmtree)\b|`"
    ),
    "ruby": re.compile(
        r"\bFile\.(?:write|open|new|delete|unlink|rename|symlink|link|truncate|chmod|"
        r"binwrite)\b|\bIO\.(?:write|binwrite|popen|sysopen)\b|\bFileUtils\b|"
        r"\bDir\.(?:mkdir|rmdir|delete|unlink)\b|\bPathname\b|\bOpen3\b|"
        r"\b(?:system|exec|spawn)\b|`|%x|\.write\s*\("
    ),
    "awk": re.compile(r"\bprintf?\b[^;}\n]*?(?:>|\|)|\bsystem\s*\("),
}

_ABS_LIT = re.compile(r"(?<![\w.:/~}$\\-])/[\w.@%+=,:~/-]+")
_VAR_LIT = re.compile(r"(?<![\w$])(?:~|\$\{?[A-Za-z_][A-Za-z0-9_]*\}?)/[\w.@%+=,:~/${}-]*")
_STR_LIT = re.compile(r"""'([^'\n]*)'|"([^"\n]*)"|`([^`\n]*)`""")
_REL_PATH = re.compile(r"^(?:\.{1,2}/)?[\w.@+-]+(?:/[\w.@+-]+)*/?$")
_HAS_EXT = re.compile(r"\.[A-Za-z][A-Za-z0-9]{0,7}$")


def _payload_paths(code: str) -> list[str]:
    """Every literal in an interpreter payload that could name a filesystem
    path: absolute paths and `~/…`/`$VAR/…` anywhere in the text (including
    inside a string handed to os.system), plus quoted relative names that carry
    a `/` or a file extension."""
    out = _ABS_LIT.findall(code) + _VAR_LIT.findall(code)
    for m in _STR_LIT.finditer(code):
        s = (m.group(1) or m.group(2) or m.group(3) or "").strip()
        if s and _REL_PATH.match(s) and ("/" in s or _HAS_EXT.search(s)):
            out.append(s)
    return out


def _quoted_substitutions(word: str) -> list[str]:
    """Bodies of `$( … )` and `` `…` `` left inside one token (they were quoted,
    so the tokenizer did not split them out). Unbalanced -> the rest of the
    word, which then fails to tokenize and is refused."""
    out: list[str] = []
    i = 0
    while i < len(word):
        if word.startswith("$(", i) and not word.startswith("$((", i):
            depth, j = 1, i + 2
            while j < len(word) and depth:
                depth += {"(": 1, ")": -1}.get(word[j], 0)
                j += 1
            out.append(word[i + 2 : j - 1] if depth == 0 else word[i + 2 :])
            i = j
        elif word[i] == "`":
            j = word.find("`", i + 1)
            if j < 0:
                break  # a lone backquote from an unquoted `a b` split: not ours
            out.append(word[i + 1 : j])
            i = j + 1
        else:
            i += 1
    return [s for s in out if s.strip()]


def _operands(args: list[str]) -> list[str]:
    return [a for a in args if not a.startswith("-")]


class _Analyzer:
    def __init__(self, root: str):
        self.root = root
        self._own: tuple[bool, str | None] = (False, None)

    # -- target judgement ---------------------------------------------------
    def own_gitdir(self) -> str | None:
        if not self._own[0]:
            self._own = (True, _own_worktree_gitdir(self.root))
        return self._own[1]

    def check(self, target: str, st: _State) -> None:
        if target == "-":
            return  # stdout
        if _hits_main(self.root, target, self.own_gitdir(), st):
            raise _Hit(target)

    # -- text ---------------------------------------------------------------
    def analyze(self, text: str, st: _State, depth: int) -> _State:
        """Judge a whole command text; return the shell state it leaves."""
        if depth > MAX_DEPTH:
            raise _Undet(f"command wrappers nested more than {MAX_DEPTH} levels")
        stripped, bodies = _strip_heredoc_bodies(text)
        tokens = _tokenize(stripped)
        if tokens is None:
            if depth == 0:
                raise _Unparseable()
            raise _Undet("a nested command payload does not tokenize")
        n_here = sum(1 for t in tokens if t == "<<")
        if n_here != len(bodies):
            # Could not line bodies up with their operators; a body that is
            # fed to an interpreter then cannot be read (judged in _stdin_code).
            bodies = [None] * n_here
        return _Walk(self, st, bodies, depth).run(tokens)


class _Walk:
    """One pass over a token stream, tracking cwd and variables segment by
    segment and judging each simple command's effect."""

    def __init__(self, an: _Analyzer, st: _State, bodies: list, depth: int):
        self.an = an
        self.st = st
        self.bodies = bodies
        self.here = 0
        self.depth = depth
        # State that holds instead if the current `&&`/`||` chain stops early.
        self.fallback: _State | None = None
        self.blocks: list[_State] = []
        self.find_cont: list[str] | None = None

    # -- driver -------------------------------------------------------------
    def run(self, tokens: list[str]) -> _State:
        stack: list[tuple] = []
        cur: list[str] = []
        prev: str | None = None
        for tok in tokens + [None]:
            if tok is not None and tok not in SEPARATORS:
                cur.append(tok)
                continue
            if tok in ("(", ")") and cur and os.path.basename(cur[0]) == "find":
                cur.append(tok)  # `find … \( … \)`: an argument, not syntax
                continue
            if tok in ("(", "<(", ">("):
                subst = tok == "(" and bool(cur) and cur[-1].endswith("$")
                if subst or tok != "(":
                    # `$( … )` / `<( … )`: judged on its own, continues the
                    # enclosing command with an unresolvable word in its place.
                    if subst:
                        cur[-1] = cur[-1][:-1] + SUBST_WORD
                    else:
                        cur.append(SUBST_WORD)
                    stack.append(("cont", cur, self.st.copy(), prev, self.fallback))
                else:
                    if cur:
                        self.segment(cur, prev, tok)
                    stack.append(("sub", None, self.st.copy(), prev, self.fallback))
                cur, prev, self.fallback = [], None, None
                continue
            if tok == ")":
                if cur:
                    self.segment(cur, prev, None)
                self.end_chain()
                cur = []
                if stack:
                    kind, saved, st, prev, fb = stack.pop()
                    # A subshell / substitution's cwd and variables do not leak.
                    self.st, self.fallback = st, fb
                    if kind == "cont":
                        cur = saved
                continue
            if cur:
                self.segment(cur, prev, tok)
            cur = []
            if tok in _CHAIN_END:
                self.end_chain()
            prev = tok
        while self.blocks:  # an unterminated block: either path may have held
            self.st = _merge(self.st, self.blocks.pop())
        return self.st

    def end_chain(self) -> None:
        if self.fallback is not None:
            self.st = _merge(self.st, self.fallback)
            self.fallback = None

    # -- one simple command -------------------------------------------------
    def segment(self, seg: list[str], prev: str | None, nxt: str | None) -> None:
        if prev == "||" and self.fallback is not None:
            # Runs only if something earlier in the chain FAILED, so it may see
            # the state from before that something.
            self.st = _merge(self.st, self.fallback)
        pre = self.st
        # A `$( … )` or backquote INSIDE a quoted word reaches here as part of
        # one token; its command still runs, so judge it (in a subshell state).
        for w in seg:
            for inner in _quoted_substitutions(w):
                self.an.analyze(inner, pre.copy(), self.depth + 1)
        argv, stdin = self.redirects(seg, pre)

        # Compound-command keywords: track blocks, then judge what follows.
        while argv and argv[0] in LEAD_KEYWORDS:
            kw = argv.pop(0)
            if kw in ("if", "while", "until"):
                self.blocks.append(self.st.copy())
            elif kw in BLOCK_CLOSE and self.blocks:
                self.st = _merge(self.st, self.blocks.pop())
            elif kw in ("else", "elif") and self.blocks:
                self.st = _merge(self.st, self.blocks[-1])
        if argv and argv[0] in ("for", "select", "case"):
            self.blocks.append(self.st.copy())
        if not argv:
            return

        if self.find_cont is not None and argv[0].startswith("-"):
            # The tail of `find … -exec cmd {} \; -delete` after the `;`.
            self.find_expr(self.find_cont, argv, self.st)
        self.find_cont = None

        pre = self.st
        new = self.judge(argv, pre, stdin, nxt)
        if new is None:
            return
        if prev in ("|", "|&") or nxt in ("|", "|&", "&"):
            return  # a pipeline stage / background job runs in a subshell
        if prev in ("&&", "||") or nxt == "||":
            # Conditional (or followed by a branch that runs only if it failed):
            # whatever comes later may still see the state from before it.
            self.fallback = pre if self.fallback is None else _merge(self.fallback, pre)
        self.st = new

    def redirects(self, seg: list[str], st: _State) -> tuple[list[str], tuple | None]:
        argv: list[str] = []
        stdin: tuple | None = None
        i = 0
        while i < len(seg):
            t = seg[i]
            nxt = seg[i + 1] if i + 1 < len(seg) else None
            if t in REDIR_OUT:
                if nxt is not None:
                    self.an.check(nxt, st)
                i += 2
                continue
            if t == ">&":
                if nxt is not None and not (nxt.isdigit() or nxt == "-"):
                    self.an.check(nxt, st)  # `>& file` is `&> file`
                i += 2
                continue
            if t in ("<", "<&"):
                if t == "<" and nxt is not None:
                    stdin = ("file", nxt)
                i += 2
                continue
            if t == "<<":
                body = self.bodies[self.here] if self.here < len(self.bodies) else None
                self.here += 1
                stdin = ("code", body)
                i += 2
                continue
            if t == "<<<":
                stdin = ("code", nxt)
                i += 2
                continue
            argv.append(t)
            i += 1
        return argv, stdin

    # -- dispatch ------------------------------------------------------------
    def judge(self, argv: list[str], st: _State, stdin: tuple | None,
              nxt: str | None = None) -> _State | None:
        """Judge one simple command. Returns the new shell state if the command
        changes it (cd, assignment), else None."""
        k = 0
        while k < len(argv) and _ASSIGN.match(argv[k]):
            k += 1
        if k == len(argv):
            new = st.copy()
            for a in argv:
                m = _ASSIGN.match(a)
                assert m is not None
                new.vars[m.group(1)] = _expand(m.group(2), new)
            return new
        argv = argv[k:]  # prefix assignments only reach the command's environment

        word = _expand(argv[0].lstrip("`"), st)
        if any(c in word for c in "$`"):
            if self.depth > 0 or any(re.match(r"^-\w*[ce]$", a) for a in argv[1:]):
                raise _Undet(f"cannot tell which program `{argv[0]}` runs")
            return None
        prog = os.path.basename(word)
        rest = argv[1:]

        if prog in ("cd", "pushd"):
            return self.cd(rest, st, nxt)
        if prog == "popd":
            return _State(None, None, dict(st.vars))
        if prog in ("export", "declare", "typeset", "local", "readonly"):
            new = st.copy()
            for a in rest:
                m = _ASSIGN.match(a)
                if m:
                    new.vars[m.group(1)] = _expand(m.group(2), new)
            return new
        if prog == "unset":
            new = st.copy()
            for a in _operands(rest):
                new.vars[a] = ""
            return new
        if prog in ("read", "mapfile", "readarray", "getopts", "for", "select"):
            new = st.copy()
            for a in _operands(rest):
                if re.match(r"^[A-Za-z_][A-Za-z0-9_]*$", a):
                    new.vars[a] = UNKNOWN_VAL
                if prog in ("for", "select"):
                    break  # only the loop variable
            return new
        if prog == "case":
            return None

        if prog == "env":
            return self.env(rest, st, stdin)
        if prog in WRAPPERS:
            return self.wrapper(prog, rest, st, stdin, nxt)
        if prog == "eval":
            return self.an.analyze(" ".join(rest), st.copy(), self.depth + 1)
        if prog in SHELLS:
            self.shell(rest, st, stdin)
            return None
        if _PYTHON.match(prog):
            self.python(rest, st, stdin)
            return None
        if prog in ("node", "nodejs", "bun", "deno"):
            self.node(prog, rest, st, stdin)
            return None
        if prog in ("perl", "ruby"):
            self.perl_ruby(prog, rest, st, stdin)
            return None
        if prog in AWKS:
            self.awk(rest, st)
            return None

        if prog in TARGET_ALL:
            for a in _operands(rest):
                self.an.check(a, st)
        elif prog in TARGET_AFTER_FIRST:
            for a in _operands(rest)[1:]:
                self.an.check(a, st)
        elif prog in TARGET_LAST:
            ops = _operands(rest)
            if ops:
                self.an.check(ops[-1], st)
        elif prog in ("sed", "gsed"):
            self.sed(rest, st)
        elif prog == "dd":
            for a in rest:
                if a.startswith("of="):
                    self.an.check(a[3:], st)
        elif prog == "curl":
            self.curl(rest, st)
        elif prog == "wget":
            self.wget(rest, st)
        elif prog in ("tar", "gtar", "bsdtar"):
            self.tar(rest, st)
        elif prog == "unzip":
            self.unzip(rest, st)
        elif prog == "find":
            starts: list[str] = []
            j = 0
            while j < len(rest) and not (rest[j].startswith("-") or rest[j] in ("(", "!", ")")):
                starts.append(rest[j])
                j += 1
            starts = starts or ["."]
            self.find_expr(starts, rest[j:], st)
            if nxt in (";",) and any(a in ("-exec", "-execdir", "-ok", "-okdir") for a in rest):
                self.find_cont = starts
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
        # `git -C <dir>` re-anchors only that one git command: it no longer
        # excuses the rest of the command line.
        return None

    # -- state changers --------------------------------------------------------
    def cd(self, rest: list[str], st: _State, nxt: str | None) -> _State:
        ops = [a for a in rest if a not in ("-P", "-L", "-e", "-@", "--")]
        if not ops:
            dest = st.vars.get("HOME", os.environ.get("HOME") or UNKNOWN_VAL)
        elif ops[0] == "-":
            return _State(None, None, dict(st.vars))  # $OLDPWD: not tracked
        else:
            dest = _expand(ops[0], st)
        if any(c in dest for c in _UNRESOLVABLE) or (not os.path.isabs(dest) and st.rel is None):
            return _State(None, None, dict(st.vars))
        resolved = _resolve(st.rel or self.an.root, dest)
        new = _State(resolved, resolved, dict(st.vars))
        if nxt != "&&" and not os.path.isdir(resolved):
            # The cd may fail, leaving the cwd where it was, and the next command
            # runs anyway.
            return _merge(st, new)
        return new

    # -- wrappers --------------------------------------------------------------
    def env(self, rest: list[str], st: _State, stdin) -> None:
        st2 = st
        j = 0
        while j < len(rest):
            a = rest[j]
            if a in ("-u", "--unset") and j + 1 < len(rest):
                j += 2
                continue
            if a in ("-C", "--chdir") and j + 1 < len(rest):
                st2 = self.cd([rest[j + 1]], st2, "&&")
                j += 2
                continue
            if a in ("-S", "--split-string") and j + 1 < len(rest):
                self.an.analyze(" ".join(rest[j + 1:]), st2.copy(), self.depth + 1)
                return None
            if a.startswith("-") or _ASSIGN.match(a):
                j += 1
                continue
            break
        if j < len(rest):
            self.judge(rest[j:], st2, stdin)
        return None

    def wrapper(self, prog: str, rest: list[str], st: _State, stdin,
                nxt: str | None) -> _State | None:
        arg_opts = WRAPPERS[prog]
        j = 0
        positional = 1 if prog in ("timeout", "gtimeout") else 0
        st2 = st
        while j < len(rest):
            a = rest[j]
            if a == "--":
                j += 1
                break
            if a.startswith("-") and len(a) > 1:
                if prog == "command" and a in ("-v", "-V"):
                    return None  # a lookup, runs nothing
                if a in arg_opts and j + 1 < len(rest):
                    if prog == "time" and a == "-o":
                        self.an.check(rest[j + 1], st)
                    if prog == "sudo" and a == "-D":
                        st2 = self.cd([rest[j + 1]], st2, "&&")
                    j += 2
                    continue
                j += 1
                continue
            if positional:
                positional -= 1
                j += 1
                continue
            break
        inner = rest[j:]
        if not inner:
            return None
        if prog == "xargs":
            inner_prog = os.path.basename(_expand(inner[0], st))
            if inner_prog in STDIN_MUTATORS:
                # Its targets arrive on stdin; the ones this process can place
                # are relative to the cwd.
                self.an.check(".", st2)
        new = self.judge(inner, st2, stdin, nxt)
        return new if prog in SAME_SHELL_WRAPPERS else None

    def _stdin_code(self, stdin, what: str) -> str | None:
        """The code an interpreter reads from stdin, if this process can see it."""
        if stdin is None or stdin[0] != "code":
            return None  # a pipe or a file: not visible here (documented residual)
        if stdin[1] is None:
            raise _Undet(f"{what} reads a here-document this gate could not isolate")
        return stdin[1]

    def shell(self, rest: list[str], st: _State, stdin) -> None:
        has_c = False
        j = 0
        while j < len(rest):
            a = rest[j]
            if a == "--":
                j += 1
                break
            if a[:1] in ("-", "+") and len(a) > 1:
                if a.startswith("--"):
                    if a == "--command":  # fish
                        has_c = True
                    j += 1
                    continue
                if "c" in a[1:] and a[0] == "-":
                    has_c = True
                if ("o" in a[1:] or "O" in a[1:]) and j + 1 < len(rest):
                    j += 2
                    continue
                j += 1
                continue
            break
        if has_c:
            if j < len(rest):
                self.an.analyze(rest[j], st.copy(), self.depth + 1)
            return
        if j < len(rest):
            return  # a script FILE: its content is not judged (residual)
        code = self._stdin_code(stdin, "the shell")
        if code is not None:
            self.an.analyze(code, st.copy(), self.depth + 1)

    def scan(self, lang: str, code: str, args: list[str], st: _State) -> None:
        """Judge interpreter source that cannot be parsed as shell: if it
        contains a write primitive, every literal path in it (and every operand
        passed to it) is a write-target candidate. A write with no literal path
        at all cannot be placed, so it is refused (3.)."""
        writes = bool(_WRITE[lang].search(code))
        if lang == "python" and re.search(r"\bopen\s*\(", code) and _PY_MODE.search(code):
            writes = True
        if not writes:
            return
        cands = _payload_paths(code) + _operands(args)
        if not cands:
            raise _Undet(f"a {lang} payload writes files, but names no literal path")
        for c in cands:
            if _fragment_of_root(self.an.root, c):
                raise _Hit(c)
            self.an.check(c, st)

    def python(self, rest: list[str], st: _State, stdin) -> None:
        j = 0
        while j < len(rest):
            a = rest[j]
            if a == "-":
                break
            if a.startswith("-") and not a.startswith("--") and "c" in a[1:]:
                idx = a.index("c", 1)
                code = a[idx + 1:] or (rest[j + 1] if j + 1 < len(rest) else "")
                args = rest[j + 1:] if a[idx + 1:] else rest[j + 2:]
                self.scan("python", code, args, st)
                return
            if a.startswith("-") and len(a) > 1:
                if a in ("-m",):
                    return  # a module: not visible here (residual)
                if a in ("-W", "-X") and j + 1 < len(rest):
                    j += 2
                    continue
                j += 1
                continue
            return  # a script file (residual)
        code = self._stdin_code(stdin, "python")
        if code is not None:
            self.scan("python", code, rest[j + 1:], st)

    def node(self, prog: str, rest: list[str], st: _State, stdin) -> None:
        j = 0
        if prog == "deno":
            if rest and rest[0] == "eval" and len(rest) > 1:
                self.scan("node", rest[1], rest[2:], st)
            return
        while j < len(rest):
            a = rest[j]
            if a.startswith("--eval=") or a.startswith("--print="):
                self.scan("node", a.split("=", 1)[1], rest[j + 1:], st)
                return
            if a in ("-e", "-p", "--eval", "--print", "-pe"):
                if j + 1 < len(rest):
                    self.scan("node", rest[j + 1], rest[j + 2:], st)
                return
            if a in ("-r", "--require", "--import", "--loader") and j + 1 < len(rest):
                j += 2
                continue
            if a.startswith("-") and a != "-":
                j += 1
                continue
            if a != "-":
                return  # a script file (residual)
            break
        code = self._stdin_code(stdin, prog)
        if code is not None:
            self.scan("node", code, [], st)

    def perl_ruby(self, prog: str, rest: list[str], st: _State, stdin) -> None:
        codes: list[str] = []
        inplace = False
        st2 = st
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a == "--":
                break
            if not a.startswith("-") or a == "-":
                j -= 1
                break
            if a.startswith("--"):
                continue
            b = a[1:]
            q = 0
            while q < len(b):
                ch = b[q]
                q += 1
                if ch in "eE":
                    if b[q:]:
                        codes.append(b[q:])
                    elif j < len(rest):
                        codes.append(rest[j])
                        j += 1
                    break
                if ch == "i":
                    inplace = True
                    break  # the rest of the bundle is the backup suffix
                if ch in "l0":
                    while q < len(b) and b[q].isdigit():
                        q += 1
                    continue
                if ch in "IMmrFxdDC":
                    val = b[q:]
                    if not val and ch in "IMmrC" and j < len(rest):
                        val = rest[j]
                        j += 1
                    if ch == "C" and prog == "ruby":
                        st2 = self.cd([val], st2, "&&")
                    break
        ops = rest[j:]
        if inplace:
            for a in ops:
                self.an.check(a, st2)
        lang = prog
        if codes:
            self.scan(lang, "\n".join(codes), ops, st2)
            return
        if ops:
            return  # a script file (residual)
        code = self._stdin_code(stdin, prog)
        if code is not None:
            self.scan(lang, code, [], st2)

    def awk(self, rest: list[str], st: _State) -> None:
        program: str | None = None
        progfile = False
        inplace = False
        j = 0
        while j < len(rest):
            a = rest[j]
            if a == "--":
                j += 1
                break
            if not a.startswith("-") or a == "-":
                break
            if a in ("-f", "--file"):
                progfile = True
                j += 2
                continue
            if a in ("-e", "--source") and j + 1 < len(rest):
                program = (program or "") + "\n" + rest[j + 1]
                j += 2
                continue
            if a in ("-i", "--include") and j + 1 < len(rest):
                inplace = inplace or rest[j + 1] in ("inplace", "inplace.awk")
                j += 2
                continue
            if a in ("-v", "-F", "--assign", "--field-separator") and j + 1 < len(rest):
                j += 2
                continue
            if a.startswith("-f"):
                progfile = True
            j += 1
        ops = rest[j:]
        if program is None and not progfile and ops:
            program, ops = ops[0], ops[1:]
        files = [a for a in ops if not _ASSIGN.match(a)]
        if inplace:
            for a in files:
                self.an.check(a, st)
        if program is not None:
            self.scan("awk", program, [], st)

    # -- file tools -----------------------------------------------------------
    def sed(self, rest: list[str], st: _State) -> None:
        inplace = False
        has_expr = False
        ops: list[str] = []
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a == "--":
                ops += rest[j:]
                break
            if a.startswith("--"):
                name = a[2:].split("=", 1)[0]
                if name == "in-place":
                    inplace = True
                elif name in ("expression", "file"):
                    has_expr = True
                    if "=" not in a:
                        j += 1
                continue
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch == "i":
                        inplace = True
                        if q == len(b) - 1 and j < len(rest) and (
                            rest[j] == "" or rest[j].startswith(".")
                        ):
                            j += 1  # BSD `-i ''` / `-i .bak`: the suffix operand
                        break  # GNU: the rest of the bundle is the suffix
                    if ch in "ef":
                        has_expr = True
                        if q == len(b) - 1:
                            j += 1  # the script / script file is the next word
                        break
                    if ch == "l" and q == len(b) - 1:
                        j += 1
                        break
                continue
            ops.append(a)
        if not inplace:
            return
        # `sed -i 's/a/b/' f1 f2` — the FIRST operand is the script, not a file,
        # unless the script came via -e/-f (then all operands are files).
        for a in (ops if has_expr else ops[1:]):
            self.an.check(a, st)

    def curl(self, rest: list[str], st: _State) -> None:
        write_long = {"output", "dump-header", "cookie-jar", "trace", "trace-ascii",
                      "stderr", "libcurl", "etag-save", "hsts", "alt-svc"}
        arg_short = set("AbCdeEFHKmPQrTuUwxXyYzt")
        out_dir: str | None = None
        remote = False
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a.startswith("--"):
                name, eq, val = a[2:].partition("=")
                if name in write_long or name == "output-dir":
                    if not eq and j < len(rest):
                        val = rest[j]
                        j += 1
                    if name == "output-dir":
                        out_dir = val
                    else:
                        self.an.check(val, st)
                elif name in ("remote-name", "remote-name-all", "remote-header-name"):
                    remote = True
                continue
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch in "oDc":
                        val = b[q + 1:]
                        if not val and j < len(rest):
                            val = rest[j]
                            j += 1
                        self.an.check(val, st)
                        break
                    if ch in arg_short:
                        if q == len(b) - 1:
                            j += 1
                        break
                    if ch in "OJ":
                        remote = True
        if remote:
            self.an.check(out_dir or ".", st)

    def wget(self, rest: list[str], st: _State) -> None:
        arg_short = set("eiBtTwQUlDRAIX")
        doc: str | None = None
        prefix: str | None = None
        spider = False
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a.startswith("--"):
                name, eq, val = a[2:].partition("=")
                if name in ("output-document", "output-file", "append-output",
                            "directory-prefix"):
                    if not eq and j < len(rest):
                        val = rest[j]
                        j += 1
                    if name == "output-document":
                        doc = val
                    elif name == "directory-prefix":
                        prefix = val
                    else:
                        self.an.check(val, st)
                elif name == "spider":
                    spider = True
                continue
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch in "OoaP":
                        val = b[q + 1:]
                        if not val and j < len(rest):
                            val = rest[j]
                            j += 1
                        if ch == "O":
                            doc = val
                        elif ch == "P":
                            prefix = val
                        else:
                            self.an.check(val, st)
                        break
                    if ch in arg_short:
                        if q == len(b) - 1:
                            j += 1
                        break
        if spider:
            return
        if doc is not None:
            self.an.check(doc, st)
        else:
            self.an.check(prefix or ".", st)

    def tar(self, rest: list[str], st: _State) -> None:
        arg_short = set("fCbLNTVgKXIH")
        mode: set[str] = set()
        dests: list[str] = []
        archive: str | None = None
        j = 0
        if rest and not rest[0].startswith("-"):
            # Old-style `tar xzf a.tgz -C dir`: argument letters take the
            # following words in order.
            pending = [ch for ch in rest[0] if ch in arg_short]
            mode |= set(rest[0])
            j = 1
            for ch in pending:
                if j < len(rest):
                    if ch == "f":
                        archive = rest[j]
                    elif ch == "C":
                        dests.append(rest[j])
                    j += 1
        while j < len(rest):
            a = rest[j]
            j += 1
            if a.startswith("--"):
                name, eq, val = a[2:].partition("=")
                if name in ("extract", "get"):
                    mode.add("x")
                elif name in ("create", "append", "update", "catenate", "concatenate",
                              "delete"):
                    mode.add("c")
                elif name in ("directory", "file"):
                    if not eq and j < len(rest):
                        val = rest[j]
                        j += 1
                    if name == "directory":
                        dests.append(val)
                    else:
                        archive = val
                continue
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch in arg_short:
                        val = b[q + 1:]
                        if not val and j < len(rest):
                            val = rest[j]
                            j += 1
                        if ch == "f":
                            archive = val
                        elif ch == "C":
                            dests.append(val)
                        break
                    mode.add(ch)
        if "x" in mode:
            for d in dests or ["."]:
                self.an.check(d, st)
        if mode & set("cruA") and archive:
            self.an.check(archive, st)

    def unzip(self, rest: list[str], st: _State) -> None:
        dest: str | None = None
        j = 0
        while j < len(rest):
            a = rest[j]
            j += 1
            if a.startswith("-") and len(a) > 1:
                b = a[1:]
                for q, ch in enumerate(b):
                    if ch in "ltvpzZ":
                        return  # list / test / pipe: writes nothing
                    if ch == "d":
                        dest = b[q + 1:]
                        if not dest and j < len(rest):
                            dest = rest[j]
                            j += 1
                        break
                    if ch == "x":
                        break
        self.an.check(dest or ".", st)

    def find_expr(self, starts: list[str], expr: list[str], st: _State) -> None:
        j = 0
        while j < len(expr):
            a = expr[j]
            j += 1
            if a == "-delete":
                for s in starts:
                    self.an.check(s, st)
            elif a in ("-fprint", "-fprint0", "-fls", "-fprintf") and j < len(expr):
                self.an.check(expr[j], st)
                j += 1
            elif a in ("-exec", "-execdir", "-ok", "-okdir"):
                inner: list[str] = []
                while j < len(expr) and expr[j] not in (";", "+"):
                    inner.append(expr[j])
                    j += 1
                j += 1
                if inner:
                    for s in starts:
                        self.judge([s if w == "{}" else w for w in inner], st, None)


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

DENY_UNDETERMINED = """Refused: could not determine what `{cmd}` writes to.

Reason: {why}.

The command hands code to an interpreter or wrapper (sh -c, eval, python -c,
node -e, awk, …) in a form this gate cannot place on the filesystem. "Could not
determine" is not "does not touch main" (CLAUDE.md 最上位の方針 3), so it
resolves to a refusal rather than an allow.

Rewrite it so the written path appears literally (or `cd <worktree> && …`
first), or do the work in a worktree.
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

    an = _Analyzer(root)
    start = _State(root, os.getcwd(), {})
    try:
        an.analyze(command, start, 0)
    except _Unparseable:
        # The command does not tokenize, so its filesystem targets are unknown —
        # which is not the same as "it has none". This used to allow, and a
        # here-doc body containing an apostrophe was enough to walk a write to
        # main straight past the gate.
        return 2, DENY_UNPARSEABLE.format(cmd=_first_line(command))
    except _Hit as hit:
        return 2, DENY.format(cmd=f"{_first_line(command)}` (target `{hit.args[0]}")
    except _Undet as why:
        return 2, DENY_UNDETERMINED.format(cmd=_first_line(command), why=why.args[0])
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
#   * paths REBUILT AT RUNTIME inside an interpreter payload from fragments
#     that are not themselves path-shaped (`'/Users/u/src/har' + 'ness'`,
#     `os.path.join(a, b)` with a and b computed, `chr()`-built strings, a
#     path read from a file or the environment). A payload that writes and
#     names no literal path at all is refused, but one that ALSO names an
#     unrelated literal path outside main is judged on that literal and
#     allowed. Single-quoted shell text inside a payload (`'$S'`) is expanded
#     as if the shell had expanded it.
#   * code this hook cannot read: a script FILE (`bash x.sh`, `python3 x.py`,
#     `node x.js`, `source x`, `python3 -m mod`), code piped on stdin
#     (`curl … | sh`, `cat x | python3`), `osascript -e`, `php -r`, and any
#     other interpreter not listed in the module docstring.
#   * a program whose NAME is unresolvable at the top level (`$PY -c …`, unless
#     its arguments look like `-c`/`-e`) — inside a nested payload it is refused.
#   * write primitives this hook does not list (Python's `Path.replace`, a
#     write through a library call, `sed 's/a/b/w file'`, a here-string fed to
#     an unlisted interpreter), and `xargs` / `find -exec` targets that arrive
#     ABSOLUTE on stdin (relative ones are judged against the cwd).
#   * a tool that resolves its own paths (a Makefile, cargo, `git` subcommands
#     — see the NOTE in _Walk.judge), and a function body or alias defined and
#     called in the same command.
#   * shlex loses quoting, so a quoted `';'`, `'>'` or `'$(…)'` is read as shell
#     syntax: that only ever OVER-refuses (an extra separator / redirect /
#     payload), it cannot hide a write that is spelled literally.
#   All of these are caught at the durable moment by check-worktree-isolation.py,
#   which refuses the commit regardless of how the tree was dirtied.
