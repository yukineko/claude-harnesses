#!/usr/bin/env python3
"""check-unlisted-gates — a gate scanner in this checkout that no active hook runs.

Backlog 666b6704. `core.hooksPath` is shared by every linked worktree, and in
this repository it is the main tree's ABSOLUTE `.githooks`. So the hook BODY git
executes (which scanners are called) always comes from main, while the hook
resolves each scanner FILE as `$(git rev-parse --show-toplevel)/scripts/<name>`,
i.e. from the current checkout. A scanner added in a worktree is therefore never
run until merged: the commit passes silently. Deleting a scanner is caught (the
hook's `run` helper blocks on a missing file); adding one was not. This scanner
makes the addition red.

User ruling (2026-10-03): keep the hook path pointing at main's list, and BLOCK
when a scanner present in the checkout's scripts/ is not invoked by the active
hooks. A work-in-progress gate is stopped until it is merged and wired. A relative
hook path and a document-only fix were rejected. There is deliberately NO
allowlist: a scanner that exists must be run by some hook, or it is a gate that
blocks nothing.

What "the active hooks" means
-----------------------------
Exactly what git would execute from this cwd:
  * `git config --type=path --get core.hooksPath`; absolute as-is, relative
    joined to the toplevel (git runs hooks from the toplevel of a non-bare
    checkout, so that is where a relative hooksPath resolves);
  * unset -> `<git common dir>/hooks`.
`--hooks-dir DIR` (backlog 52f85060) replaces that resolution with DIR, read
exactly as above; it exists so a scratch clone whose core.hooksPath is unset
can be checked against its own `.githooks` without writing git config. A
relative DIR resolves against the invoking cwd (not `--repo`). DIR missing, not
a directory, or unstat-able is undetermined (exit 2), never an empty hook set.
`pre-commit` in that directory must exist, be readable and be executable (git
skips a non-executable hook), else the verdict is undetermined. Every OTHER
standard git hook name present there (pre-push, commit-msg, ...) is read too, so
a scanner that legitimately runs only at push or commit-msg time counts as
listed. A sibling hook that exists but cannot be read is undetermined; one
without the exec bit is skipped, because git never runs it.

Claude Code hooks and invocation chains (backlog 96e2fde0)
---------------------------------------------------------
The project's checked-in `<toplevel>/.claude/settings.json` hooks are active
hooks too (e.g. SessionStart -> scripts/session-worktree-init.py ->
scripts/session-rollout-drift.py -> scripts/check-plugin-rollout.py). Every
`type: "command"` hook's command string is parsed with the same shell parser
as a git hook body. Still NO allowlist: a scanner counts only when an active
hook REACHES it by tracing real invocations:
  * the roots are every `scripts/<name>.py|.sh` (check-* or not) invoked, in
    the shapes below, by a git hook body or a settings.json command;
  * a reached `.sh` is read with the same shell parser; a reached `.py` is
    read with `ast` (python_invokes(): a subprocess / runner call whose argv
    literal runs `scripts/<name>` or a `__file__`-sibling `<name>`, in code
    reachable from module top level); the closure is followed transitively;
  * a scanner nothing reaches stays unlisted (red).
settings.json missing = no project hooks (absence). settings.json unreadable,
not JSON, or with a malformed `hooks` shape, a command that cannot be parsed,
and a reached script that cannot be read or parsed are undetermined (exit 2).
`disableAllHooks: true` counts no Claude hook. `settings.local.json` and
user-level settings are not read. Python shapes other than those listed in
python_invokes() (os.system, os.exec*, shell=True strings, imports, computed
paths, ...) are NOT followed, so a scanner reached only that way reads as
unlisted — the red direction.

Naming rule for "gate scanner"
------------------------------
A regular file directly in `<toplevel>/scripts/` whose name matches
`check-*.py` or `check-*.sh`:
  * every scanner the hooks call today is `scripts/check-*.py` (pre-commit's
    `run` helper hard-codes `$REPO/scripts/$scanner`; commit-msg and pre-push
    name `scripts/check-*.py` directly);
  * `.sh` is included because `check-unwind.sh` exists under the same prefix
    (as did `check-versions.sh`, removed under eca8dea7); excluding them would
    let a shell scanner stay dark;
  * not included: `test_check_*.py` (tests of scanners), `check-*.baseline`
    (scanner data), anything under `scripts/tests/` (shell test harnesses),
    and helpers without the `check-` prefix (`gate-bypass.py` etc.), none of
    which is a gate a hook is expected to run.

What counts as an invocation
----------------------------
Each hook body is walked as ONE stream with shell quote state carried across
lines (`command_starts`): single quotes, double quotes with backslash escapes,
backslash-newline continuations, `${...}`, `$(...)`, backticks, subshells,
comments (only outside quotes) and heredoc bodies (skipped as data). So a
remediation hint that merely *mentions* `python3 scripts/x.py` — on one line or
on the continuation line of a multi-line "..." / '...' string — does not list x.
A `$(...)` or backtick inside a double-quoted string really executes, so a
command there still counts. An unbalanced quote / `$(` / backtick / `(` / `${`
or an unterminated heredoc at EOF means the body could not be parsed: exit 2.

Recognised at a command position (start of the body or a line, after ; & | ( )
{ } `$(` or a backtick, after `then/do/else/if/...`, after a leading VAR=value
word):
  1. `run <check-name>` — only in a hook that defines a `run()` function (the
     pre-commit helper) somewhere outside quotes/comments;
  2. `python3|python|bash|sh [-opts] <...>scripts/<check-name>`;
  3. a direct exec of `<...>scripts/<check-name>`;
  4. `VAR=<...>scripts/<check-name>` followed elsewhere in the same hook by
     `python3|python|bash|sh "$VAR"` (commit-msg's SCANNER= form).
The same four shapes are matched for ANY `scripts/<name>.py|.sh` (not only
check-*) so that a non-check helper becomes a chain root (see the Claude Code
hooks section); only check-* names in the resulting closure are "invoked".
Anything this parser does not recognise is NOT counted, so an unrecognised
invocation style reads as unlisted (red), never as listed.

Reachability (backlog 74280d46): an invocation is NOT counted when it sits
inside a function that live code never calls, in the branch of a literal
`if false` / `elif false` (or `! true`), in the body of `while false`, or after
an unconditional `exit` / `return` in the same block (one not reached through
`&&` / `||` / `|`). See command_starts().

KNOWN LIMITATION (fail-open, still open): only those syntactic shapes are
recognised. An invocation behind a computed condition that is always false
(`if [ 1 = 0 ]`), a `case` arm no value reaches, a function called only from
such a branch, and so on, is still counted. (The opposite error — a function
called only through `eval` or a variable reads as uncalled — is the red
direction.) Closing that
needs evaluation of the hook; it is stated here rather than implied away.

Usage
-----
  check-unlisted-gates.py [--repo PATH] [--hooks-dir DIR]
`--repo PATH` runs the check as if from PATH (default: cwd). `--hooks-dir DIR`
reads hooks from DIR instead of the core.hooksPath resolution. Each option at
most once; anything else is a usage error (exit 2).

Exit codes
----------
  0  every gate scanner in scripts/ is invoked; prints one OK line with the count
  1  one or more scanners are not invoked by any active hook; each is named
  2  undetermined (or usage error): not a git work tree, hooksPath unreadable,
     --hooks-dir missing / not a directory, active pre-commit
     missing/unreadable/not executable, a sibling hook unreadable, a hook body
     that cannot be parsed (unbalanced quoting / nesting, unterminated or
     unparseable heredoc), .claude/settings.json unreadable / not JSON /
     malformed hooks shape, a settings.json hook command that cannot be parsed,
     a chained script that cannot be read or parsed, scripts/
     unreadable or missing, zero gate scanners found, or zero check-* scanner
     invocations parsed from the git hooks (an empty set is not a clean set).
"""

import ast
import json
import os
import re
import stat
import subprocess
import sys

NAME = "check-unlisted-gates"

SCANNER_RE = re.compile(r"^check-[A-Za-z0-9_.-]+\.(?:py|sh)$")
CHECK_NAME = r"check-[A-Za-z0-9_.-]+\.(?:py|sh)"

# githooks(5) names. Only these are executed by git; other files in the hook dir
# (README, *.sample) are not hooks.
GIT_HOOKS = (
    "applypatch-msg", "pre-applypatch", "post-applypatch", "pre-commit",
    "pre-merge-commit", "prepare-commit-msg", "commit-msg", "post-commit",
    "pre-rebase", "post-checkout", "post-merge", "pre-push", "pre-receive",
    "update", "proc-receive", "post-receive", "post-update",
    "reference-transaction", "push-to-checkout", "pre-auto-gc", "post-rewrite",
    "sendemail-validate", "fsmonitor-watchman", "p4-changelist",
    "p4-prepare-changelist", "p4-post-changelist", "p4-pre-submit",
    "post-index-change",
)

INTERP = r"(?:exec[ \t]+|command[ \t]+)?(?:python3|python|bash|sh)[ \t]+(?:-[A-Za-z]+[ \t]+)*"
# An operand word: optionally quoted, no whitespace / shell metachar inside.
OPERAND = r"([\"']?)([^\s\"';|&()<>`]+)\1"
SCRIPT_PATH_RE = re.compile(r"^(?:\$\{?\w+\}?/|\./|[^\s\"']*/)?scripts/(" + CHECK_NAME + r")$")
VAR_REF_RE = re.compile(r"^\$\{?(\w+)\}?$")

INTERP_RE = re.compile(INTERP + OPERAND)
DIRECT_RE = re.compile(OPERAND)
RUN_RE = re.compile(r"run[ \t]+(" + CHECK_NAME + r")(?=[\s;&|)]|$)")
RUN_DEF_RE = re.compile(r"(?:function[ \t]+run\b|run[ \t]*\([ \t]*\))")
ASSIGN_RE = re.compile(r"([A-Za-z_]\w*)=")
KEYWORD_RE = re.compile(r"(?:then|do|else|elif|if|while|until|time|!)(?=[ \t\n])")
HEREDOC_RE = re.compile(
    r"<<(-?)[ \t]*(?:'([^'\n]*)'|\"([^\"\n]*)\"|\\?([A-Za-z0-9_.-]+))"
)
REDIR_RE = re.compile(r"[<>]+&?[ \t]*[^\s;&|()<>\"'`]*")
# Control-flow words (backlog 74280d46), matched only at a command position.
_END = r"(?=[\s;&|)]|$)"
CLOSER_RE = re.compile(r"(fi|done|esac)" + _END)
BRANCH_RE = re.compile(r"(then|do|else|elif)" + _END)
OPENER_RE = re.compile(r"(if|while|until|for|select|case)(?=\s)")
FALSE_COND_RE = re.compile(r"[ \t]+(?:![ \t]*true|false)[ \t]*(?:;|\n|$)")
EXIT_RE = re.compile(r"(?:exit|return)" + _END)
FUNC_DEF_RE = re.compile(
    r"(?:function[ \t]+([A-Za-z_][\w.-]*)(?:[ \t]*\([ \t]*\))?|([A-Za-z_][\w.-]*)[ \t]*\([ \t]*\))"
)
CALL_WORD_RE = re.compile(r"([A-Za-z_][\w.-]*)" + _END)


def _frame(kind):
    return {"kind": kind, "dead": False, "func": None, "pending": False, "false_cond": False}


class Undetermined(Exception):
    pass


def git(cwd, *args):
    try:
        p = subprocess.run(["git", *args], cwd=cwd, capture_output=True, text=True)
    except OSError as e:
        raise Undetermined("could not run git %s: %s" % (" ".join(args), e))
    return p


def resolve_hook_dir(cwd):
    top = git(cwd, "rev-parse", "--show-toplevel")
    if top.returncode != 0 or not top.stdout.strip():
        raise Undetermined(
            "git rev-parse --show-toplevel exited %d (not a work tree?): %s"
            % (top.returncode, top.stderr.strip())
        )
    toplevel = top.stdout.strip()

    cfg = git(cwd, "config", "--type=path", "--get", "core.hooksPath")
    if cfg.returncode == 0 and cfg.stdout.strip():
        hp = cfg.stdout.strip()
        hook_dir = hp if os.path.isabs(hp) else os.path.join(toplevel, hp)
        how = "core.hooksPath=%s" % hp
    elif cfg.returncode == 1 and not cfg.stdout.strip() and not cfg.stderr.strip():
        common = git(cwd, "rev-parse", "--path-format=absolute", "--git-common-dir")
        if common.returncode != 0 or not common.stdout.strip():
            raise Undetermined(
                "core.hooksPath unset and git rev-parse --git-common-dir exited %d: %s"
                % (common.returncode, common.stderr.strip())
            )
        hook_dir = os.path.join(common.stdout.strip(), "hooks")
        how = "core.hooksPath unset -> <git common dir>/hooks"
    else:
        raise Undetermined(
            "could not read core.hooksPath (git config exited %d): %s"
            % (cfg.returncode, cfg.stderr.strip())
        )
    return toplevel, os.path.normpath(hook_dir), how


def read_hooks(hook_dir):
    """Return {hook_name: text} for every hook git would run from hook_dir."""
    bodies = {}
    for name in GIT_HOOKS:
        path = os.path.join(hook_dir, name)
        try:
            st = os.stat(path)
        except FileNotFoundError:
            if name == "pre-commit":
                raise Undetermined("active hook %s does not exist" % path)
            continue
        except OSError as e:
            raise Undetermined("cannot stat hook %s: %s" % (path, e))
        if not stat.S_ISREG(st.st_mode):
            raise Undetermined("hook %s is not a regular file" % path)
        if not os.access(path, os.X_OK):
            if name == "pre-commit":
                raise Undetermined("active hook %s is not executable, so git does not run it" % path)
            continue  # git skips a non-executable hook: it invokes nothing
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                bodies[name] = fh.read()
        except OSError as e:
            raise Undetermined("cannot read hook %s: %s" % (path, e))
    return bodies


def _skip_heredocs(src, i, heredocs):
    """Skip the bodies of the heredocs opened on the line that ended at i-1.

    Heredoc bodies are data (remediation text etc.), never commands; this also
    applies to an unquoted delimiter, whose body could expand `$(...)` — not
    counting that is the conservative (red) direction.
    """
    n = len(src)
    for strip_tabs, delim in heredocs:
        while True:
            if i >= n:
                raise Undetermined("heredoc <<%s is never terminated" % delim)
            j = src.find("\n", i)
            line = src[i:] if j < 0 else src[i:j]
            i = n if j < 0 else j + 1
            if (line.lstrip("\t") if strip_tabs else line) == delim:
                break
    return i


def _skip_param(src, i):
    """i is at `${`; return the index after the matching `}`."""
    depth = 0
    j = i + 1
    while j < len(src):
        if src[j] == "{":
            depth += 1
        elif src[j] == "}":
            depth -= 1
            if depth == 0:
                return j + 1
        j += 1
    raise Undetermined("unterminated ${ at offset %d" % i)


def command_starts(body):
    """Return (src, starts): the indices in src where a simple command begins.

    src is body with backslash-newline continuations replaced by two spaces (same
    length). Quote state is tracked across the WHOLE body, not per line: a
    single- or double-quoted string spanning lines stays a string, so a scanner
    path on its continuation line is data. Inside double quotes only `$(` and a
    backtick open a command (they really execute there). Comments are only
    recognised outside quotes; heredoc bodies are skipped. An unbalanced quote,
    `$(`, backtick, `(`, `${` or an unterminated heredoc at EOF raises
    Undetermined: the body could not be parsed.

    Reachability (backlog 74280d46). Alongside the quote stack a stack of
    control-flow FRAMES is kept (subshell / `$(` / backtick / `{ }` / function
    body / if / loop / case). A command start is DROPPED (never returned) when
    any enclosing frame is dead:
      * after an unconditional `exit` / `return` (one not reached through
        `&&`, `||` or `|`), the rest of that frame is dead; the frame ends at
        its closer, and an if-branch / case arm also ends at `elif` / `else` /
        `;;` (`;&`, `;;&`);
      * the branch of `if false` / `elif false` and the body of `while false`
        are dead (literal `false` / `! true` condition only).
    A start inside a function body is returned with that function's name in
    its owner chain; invoked_by() counts it only if the function is called
    from live code. A `fi` / `done` / `esac` / `}` with no matching opener is
    unbalanced: Undetermined. Returns (src, starts, owners) where owners maps
    each start to its tuple of enclosing function names.
    """
    src = body.replace("\\\n", "  ")
    n = len(src)
    stack = ["top"]  # top | paren | cmdsub | bq | dq | sq
    starts = []
    owners = {}
    cmd_pos = True
    assign_depth = None  # stack depth of a leading VAR=value word being walked
    heredocs = []
    # Control-flow frames: dicts with kind, dead, func, pending (if/while
    # condition awaiting then/do), false_cond.
    frames = [_frame("top")]
    cond = False  # the next command is reached only via && / || / |
    pending_func = None
    i = 0

    def push(kind):
        nonlocal pending_func
        f = _frame(kind)
        if pending_func is not None and kind in ("brace", "paren"):
            f["func"] = pending_func
        pending_func = None
        frames.append(f)

    def pop_to(kinds, what):
        # Pop to the innermost frame of one of `kinds`, but never past a
        # quote-stack frame (paren / cmdsub / bq) that is not itself targeted.
        while len(frames) > 1:
            f = frames.pop()
            if f["kind"] in kinds:
                return f
            if f["kind"] in ("paren", "cmdsub", "bq"):
                break
        raise Undetermined("unbalanced %s at offset %d" % (what, i))

    while i < n:
        ctx = stack[-1]
        c = src[i]
        if ctx == "sq":
            if c == "'":
                stack.pop()
            i += 1
            continue
        if ctx == "dq":
            if c == "\\":
                i += 2
            elif c == '"':
                stack.pop()
                cmd_pos = False
                i += 1
            elif src.startswith("$(", i):
                stack.append("cmdsub")
                push("cmdsub")
                cmd_pos = True
                cond = False
                assign_depth = None
                i += 2
            elif c == "`":
                stack.append("bq")
                push("bq")
                cmd_pos = True
                cond = False
                assign_depth = None
                i += 1
            elif src.startswith("${", i):
                i = _skip_param(src, i)
            else:
                i += 1
            continue
        # Unquoted shell code: top level, (subshell), $(...), `...`.
        if c == "\n":
            i += 1
            if heredocs:
                i = _skip_heredocs(src, i, heredocs)
                heredocs = []
            cmd_pos = True
            cond = False
            assign_depth = None
            continue
        if c in " \t":
            if assign_depth is not None and len(stack) == assign_depth:
                # `VAR=value cmd`: the word after an assignment is a command.
                cmd_pos = True
                assign_depth = None
            i += 1
            continue
        if c == "\\":
            i += 2
            cmd_pos = False
            continue
        if c == "#" and (i == 0 or src[i - 1] in " \t\n;&|()`"):
            j = src.find("\n", i)
            i = n if j < 0 else j
            continue
        if ctx == "bq" and c == "`":
            stack.pop()
            pop_to(("bq",), "backtick")
            cmd_pos = False
            i += 1
            continue
        if src.startswith("<<", i) and not src.startswith("<<<", i):
            m = HEREDOC_RE.match(src, i)
            if not m:
                raise Undetermined("unparseable heredoc operator at offset %d" % i)
            delim = m.group(2) if m.group(2) is not None else (
                m.group(3) if m.group(3) is not None else m.group(4)
            )
            heredocs.append((m.group(1) == "-", delim))
            i = m.end()
            continue
        if c in "<>":
            i = REDIR_RE.match(src, i).end()
            continue
        if src.startswith("${", i):
            i = _skip_param(src, i)
            cmd_pos = False
            continue
        if src.startswith("$(", i):
            stack.append("cmdsub")
            push("cmdsub")
            cmd_pos = True
            cond = False
            assign_depth = None
            i += 2
            continue
        if c == "`":
            stack.append("bq")
            push("bq")
            cmd_pos = True
            cond = False
            assign_depth = None
            i += 1
            continue
        if c == "(":
            stack.append("paren")
            push("paren")
            cmd_pos = True
            cond = False
            assign_depth = None
            i += 1
            continue
        if c == ")":
            if ctx in ("paren", "cmdsub"):
                stack.pop()
                pop_to((ctx,), ")")
                cmd_pos = False
            else:
                # Unmatched at this level: a `case` pattern terminator.
                cmd_pos = True
            assign_depth = None
            i += 1
            continue
        if src.startswith("&&", i) or src.startswith("||", i):
            cmd_pos = True
            cond = True
            assign_depth = None
            i += 2
            continue
        if frames[-1]["kind"] == "case" and (src.startswith(";;", i) or src.startswith(";&", i)):
            # End of a case arm: an exit inside it does not kill the next arm.
            frames[-1]["dead"] = False
            cmd_pos = True
            cond = False
            assign_depth = None
            i += 3 if src.startswith(";;&", i) else 2
            continue
        if c == "{" and cmd_pos:
            push("brace")
            cond = False
            assign_depth = None
            i += 1
            continue
        if c == "}" and cmd_pos:
            pop_to(("brace",), "}")
            cmd_pos = True
            assign_depth = None
            i += 1
            continue
        if c in ";&|{}":
            cmd_pos = True
            cond = c == "|"
            assign_depth = None
            i += 1
            continue
        if cmd_pos:
            closer = CLOSER_RE.match(src, i)
            if closer:
                word = closer.group(1)
                pop_to({"fi": ("if",), "done": ("loop",), "esac": ("case",)}[word], word)
                cmd_pos = False
                i = closer.end()
                continue
            dead = any(f["dead"] for f in frames)
            if not dead:
                starts.append(i)
                owners[i] = tuple(f["func"] for f in frames if f["func"] is not None)
            top = frames[-1]
            branch = BRANCH_RE.match(src, i)
            if branch:
                word = branch.group(1)
                if word in ("then", "do"):
                    if top["kind"] in ("if", "loop") and top["pending"]:
                        top["pending"] = False
                        top["dead"] = top["false_cond"]
                elif top["kind"] == "if":  # elif / else start a fresh branch
                    top["dead"] = False
                    top["pending"] = word == "elif"
                    top["false_cond"] = word == "elif" and bool(FALSE_COND_RE.match(src, branch.end()))
                cond = False
                i = branch.end()
                continue
            opener = OPENER_RE.match(src, i)
            if opener:
                word = opener.group(1)
                kind = {"if": "if", "case": "case"}.get(word, "loop")
                push(kind)
                frames[-1]["pending"] = kind != "case"
                frames[-1]["false_cond"] = word in ("if", "while") and bool(
                    FALSE_COND_RE.match(src, opener.end())
                )
                cond = False
                i = opener.end()
                continue
            fdef = FUNC_DEF_RE.match(src, i)
            if fdef:
                pending_func = fdef.group(1) or fdef.group(2)
                cmd_pos = True
                i = fdef.end()
                continue
            pending_func = None
            if EXIT_RE.match(src, i) and not cond:
                top["dead"] = True
            kw = KEYWORD_RE.match(src, i)
            if kw:
                i = kw.end()
                continue
            asg = ASSIGN_RE.match(src, i)
            if asg:
                # Walk INTO the value: `x="$(python3 scripts/y.py)"` runs y.
                i = asg.end()
                cmd_pos = False
                assign_depth = len(stack)
                continue
            cmd_pos = False
        if c == '"':
            stack.append("dq")
        elif c == "'":
            stack.append("sq")
        i += 1
    if len(stack) != 1:
        raise Undetermined("unbalanced shell quoting/nesting at EOF (open: %s)" % "/".join(stack[1:]))
    if heredocs:
        raise Undetermined("heredoc <<%s is never terminated" % heredocs[0][1])
    if len(frames) != 1:
        raise Undetermined(
            "unclosed control-flow block(s) at EOF (open: %s)"
            % "/".join(f["kind"] for f in frames[1:])
        )
    return src, starts, owners


def _skip_word(src, i):
    """Skip one shell word starting at i (quote-aware); return the index after."""
    n = len(src)
    in_single = in_double = False
    depth = 0
    while i < n:
        c = src[i]
        if in_single:
            in_single = c != "'"
        elif c == "\\":
            i += 1
        elif src.startswith("$(", i):
            depth += 1
            i += 1
        elif c == ")" and depth:
            depth -= 1
        elif in_double:
            in_double = c != '"'
        elif c == '"':
            in_double = True
        elif c == "'":
            in_single = True
        elif c in " \t\n;&|" and not depth:
            break
        i += 1
    return i


def _script_name(operand, path_re=SCRIPT_PATH_RE):
    m = path_re.match(operand)
    return m.group(1) if m else None


# Any script directly under scripts/ (not only check-*): used to FOLLOW an
# invocation chain through helpers such as session-worktree-init.py.
ANY_SCRIPT_NAME = r"[A-Za-z0-9_.-]+\.(?:py|sh)"
ANY_SCRIPT_PATH_RE = re.compile(
    r"^(?:\$\{?\w+\}?/|\./|[^\s\"']*/)?scripts/(" + ANY_SCRIPT_NAME + r")$"
)
ANY_RUN_RE = re.compile(r"run[ \t]+(" + ANY_SCRIPT_NAME + r")(?=[\s;&|)]|$)")


def invoked_by(text, any_script=False):
    """Return the set of script names a shell body invokes.

    By default only check-* scanner names; with any_script=True every
    `scripts/<name>.py|.sh` invoked in a recognised shape (used to follow a
    chain through non-check helpers). Raises Undetermined when the body cannot
    be parsed (see command_starts).
    """
    path_re = ANY_SCRIPT_PATH_RE if any_script else SCRIPT_PATH_RE
    run_re = ANY_RUN_RE if any_script else RUN_RE
    src, all_starts, owners = command_starts(text)
    # A function body runs only if the function is called from live code
    # (top level, or a function that is itself live). Fixpoint over call words.
    defined = {f for chain in owners.values() for f in chain}
    live = set()
    changed = True
    while changed:
        changed = False
        for i in all_starts:
            if not set(owners[i]) <= live:
                continue
            w = CALL_WORD_RE.match(src, i)
            if w and w.group(1) in defined and w.group(1) not in live and w.group(1) not in owners[i]:
                live.add(w.group(1))
                changed = True
    starts = [i for i in all_starts if set(owners[i]) <= live]
    has_run = False
    run_calls = set()
    found = set()
    bindings = {}
    refs = set()
    for i in starts:
        if RUN_DEF_RE.match(src, i):
            has_run = True
            continue
        asg = ASSIGN_RE.match(src, i)
        if asg:
            val = src[asg.end():_skip_word(src, asg.end())].strip("\"'")
            name = _script_name(val, path_re)
            if name:
                bindings[asg.group(1)] = name
            continue
        m = run_re.match(src, i)
        if m:
            run_calls.add(m.group(1))
            continue
        m = INTERP_RE.match(src, i)
        if m:
            operand = m.group(2)
            name = _script_name(operand, path_re)
            if name:
                found.add(name)
                continue
            v = VAR_REF_RE.match(operand)
            if v:
                refs.add(v.group(1))
            continue
        m = DIRECT_RE.match(src, i)
        if m:
            name = _script_name(m.group(2), path_re)
            if name:
                found.add(name)
    if has_run:
        found |= run_calls
    for var in refs:
        if var in bindings:
            found.add(bindings[var])
    return found


# ---------------------------------------------------------------------------
# Python helpers in an invocation chain (backlog 96e2fde0)
# ---------------------------------------------------------------------------

PY_SCRIPT_LITERAL_RE = re.compile(r"^(scripts/)?(" + ANY_SCRIPT_NAME + r")$")
SUBPROCESS_FUNCS = ("run", "Popen", "call", "check_call", "check_output")
INTERP_LITERALS = ("python3", "python", "bash", "sh")
PATH_WRAPPERS = ("abspath", "realpath", "normpath")


def _is_subprocess_call(func):
    return (
        isinstance(func, ast.Attribute)
        and func.attr in SUBPROCESS_FUNCS
        and isinstance(func.value, ast.Name)
        and func.value.id == "subprocess"
    )


def _argv_arg(call):
    if call.args:
        return call.args[0]
    for kw in call.keywords:
        if kw.arg == "args":
            return kw.value
    return None


def _is_interp(node):
    if isinstance(node, ast.Attribute):
        return node.attr == "executable" and isinstance(node.value, ast.Name) and node.value.id == "sys"
    if isinstance(node, ast.Constant):
        return node.value in INTERP_LITERALS
    if isinstance(node, ast.BoolOp) and isinstance(node.op, ast.Or):
        return all(_is_interp(v) for v in node.values)
    return False


def _single_assignments(stmts):
    """{name: expr} for names assigned exactly once (plain `NAME = expr`).

    A name assigned more than once, or also bound another way (augmented /
    tuple assignment, for target, ...), maps to None: ambiguous, resolves to
    nothing (the red direction).
    """
    seen = {}

    def bind(name, value):
        seen[name] = None if name in seen else value

    for stmt in stmts:
        for node in ast.walk(stmt):
            if isinstance(node, (ast.Assign, ast.AnnAssign)):
                targets = node.targets if isinstance(node, ast.Assign) else [node.target]
                if len(targets) == 1 and isinstance(targets[0], ast.Name) and node.value is not None:
                    bind(targets[0].id, node.value)
                    continue
                other = targets
            elif isinstance(node, (ast.AugAssign, ast.For, ast.AsyncFor, ast.NamedExpr, ast.comprehension)):
                other = [node.target]
            elif isinstance(node, (ast.With, ast.AsyncWith)):
                other = [i.optional_vars for i in node.items if i.optional_vars is not None]
            else:
                continue
            for t in other:
                for sub in ast.walk(t):
                    if isinstance(sub, ast.Name):
                        bind(sub.id, None)
                        seen[sub.id] = None
    return seen


def python_invokes(text, path):
    """Return the script names (under scripts/) a Python helper really runs.

    Static and deliberately narrow — anything not in these shapes is NOT
    counted, so an unrecognised invocation reads as unlisted (red):
      * the call is `subprocess.run|Popen|call|check_call|check_output(argv,
        ...)`, or a call to a module-level "runner" function (one that passes
        its first parameter as argv to such a subprocess call), or a call
        through a parameter whose default is such a runner;
      * argv is a tuple/list literal (directly, or via a name assigned exactly
        once in that function / at module level) of the form
        `(<interp>, <script>, ...)` with <interp> = sys.executable, "python3",
        "python", "bash", "sh" or an `or` of those, or `(<script>, ...)`;
      * <script> resolves (through names assigned exactly once,
        os.path.join, os.path.abspath/realpath/normpath, str) to a literal
        "scripts/<name>", or to a bare "<name>" joined onto a path built from
        `__file__` (a sibling of the helper, which itself lives in scripts/);
      * the call sits in module top-level code or in a module-level function
        transitively called (by plain name) from top-level code.
    A mention in a message string, a comment or a docstring is never an argv,
    so it does not count. Raises Undetermined if the source cannot be parsed.
    """
    try:
        tree = ast.parse(text, filename=path)
    except (SyntaxError, ValueError) as e:
        raise Undetermined("cannot parse Python helper %s: %s" % (path, e))
    funcs = {
        n.name: n for n in tree.body if isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef))
    }
    top_stmts = [n for n in tree.body if not isinstance(n, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef))]
    module_env = _single_assignments(top_stmts)

    def first_param(fn):
        a = fn.args.posonlyargs + fn.args.args
        return a[0].arg if a else None

    runners = set()
    for name, fn in funcs.items():
        p = first_param(fn)
        if p is None:
            continue
        for node in ast.walk(fn):
            if isinstance(node, ast.Call) and _is_subprocess_call(node.func):
                arg = _argv_arg(node)
                if isinstance(arg, ast.Name) and arg.id == p:
                    runners.add(name)
                    break

    def called_names(stmts):
        out = set()
        for stmt in stmts:
            for node in ast.walk(stmt):
                if isinstance(node, ast.Call) and isinstance(node.func, ast.Name):
                    out.add(node.func.id)
        return out

    live = set()
    frontier = called_names(top_stmts) & set(funcs)
    while frontier:
        live |= frontier
        nxt = set()
        for f in frontier:
            nxt |= called_names(funcs[f].body)
        frontier = (nxt & set(funcs)) - live

    def scan(stmts, params, defaults):
        local_env = _single_assignments(stmts)

        def lookup(name):
            if name in params:
                return None
            if name in local_env:
                return local_env[name]
            return module_env.get(name)

        def resolve(node, depth=0):
            """-> (kind, name): kind 'scripts' | 'bare' | None."""
            if depth > 20:
                return None, None
            if isinstance(node, ast.Constant) and isinstance(node.value, str):
                m = PY_SCRIPT_LITERAL_RE.match(node.value)
                if not m:
                    return None, None
                return ("scripts" if m.group(1) else "bare"), m.group(2)
            if isinstance(node, ast.Name):
                v = lookup(node.id)
                return resolve(v, depth + 1) if v is not None else (None, None)
            if isinstance(node, ast.Call):
                fn = node.func
                fname = fn.attr if isinstance(fn, ast.Attribute) else (fn.id if isinstance(fn, ast.Name) else None)
                is_ospath = (
                    isinstance(fn, ast.Attribute)
                    and isinstance(fn.value, ast.Attribute)
                    and fn.value.attr == "path"
                    and isinstance(fn.value.value, ast.Name)
                    and fn.value.value.id == "os"
                )
                if is_ospath and fname == "join" and node.args and not node.keywords:
                    kind, name = resolve(node.args[-1], depth + 1)
                    if kind == "bare":
                        anchored = any(
                            isinstance(sub, ast.Name) and sub.id == "__file__"
                            for a in node.args[:-1]
                            for sub in ast.walk(a)
                        )
                        return ("scripts", name) if anchored else (None, None)
                    return kind, name
                if ((is_ospath and fname in PATH_WRAPPERS) or (isinstance(fn, ast.Name) and fname == "str")) \
                        and len(node.args) == 1 and not node.keywords:
                    return resolve(node.args[0], depth + 1)
            return None, None

        def argv_script(node):
            if isinstance(node, ast.Name):
                v = lookup(node.id)
                node = v if v is not None else node
            if not isinstance(node, (ast.Tuple, ast.List)) or not node.elts:
                return None
            elts = node.elts
            target = elts[1] if (_is_interp(elts[0]) and len(elts) >= 2) else elts[0]
            kind, name = resolve(target)
            return name if kind == "scripts" else None

        found = set()
        for stmt in stmts:
            for node in ast.walk(stmt):
                if not isinstance(node, ast.Call):
                    continue
                fn = node.func
                ok = _is_subprocess_call(fn) or (
                    isinstance(fn, ast.Name)
                    and (
                        (fn.id in runners and fn.id not in params)
                        or (fn.id in params and defaults.get(fn.id) in runners)
                    )
                )
                if not ok:
                    continue
                arg = _argv_arg(node)
                name = argv_script(arg) if arg is not None else None
                if name:
                    found.add(name)
        return found

    found = scan(top_stmts, set(), {})
    for f in live:
        fn = funcs[f]
        a = fn.args
        positional = a.posonlyargs + a.args
        params = {x.arg for x in positional + a.kwonlyargs}
        if a.vararg:
            params.add(a.vararg.arg)
        if a.kwarg:
            params.add(a.kwarg.arg)
        defaults = {}
        for p, d in zip(positional[len(positional) - len(a.defaults):], a.defaults):
            if isinstance(d, ast.Name):
                defaults[p.arg] = d.id
        for p, d in zip(a.kwonlyargs, a.kw_defaults):
            if isinstance(d, ast.Name):
                defaults[p.arg] = d.id
        found |= scan(fn.body, params, defaults)
    return found


# ---------------------------------------------------------------------------
# Claude Code hooks (.claude/settings.json) and chain following
# ---------------------------------------------------------------------------

def claude_hook_commands(toplevel):
    """Return the `command` strings of the project's Claude Code hooks.

    Reads `<toplevel>/.claude/settings.json` (the checked-in project settings).
    A missing file is a real absence (no project hooks): returns []. A file
    that exists but cannot be read, is not valid JSON, or whose `hooks` value
    does not have the documented shape is undetermined — never an empty set.
    `disableAllHooks: true` means Claude Code runs none of them: [].
    `settings.local.json` and user-level settings are not read (not part of
    the checkout); a scanner reached only from those stays unlisted (red).
    """
    path = os.path.join(toplevel, ".claude", "settings.json")
    try:
        with open(path, encoding="utf-8") as fh:
            raw = fh.read()
    except FileNotFoundError:
        return []
    except OSError as e:
        raise Undetermined("cannot read %s: %s" % (path, e))
    try:
        data = json.loads(raw)
    except ValueError as e:
        raise Undetermined("cannot parse %s as JSON: %s" % (path, e))
    if not isinstance(data, dict):
        raise Undetermined("%s is not a JSON object" % path)
    if data.get("disableAllHooks") is True:
        return []
    hooks = data.get("hooks", {})
    if not isinstance(hooks, dict):
        raise Undetermined("%s: `hooks` is not an object" % path)
    out = []
    for event, groups in hooks.items():
        if not isinstance(groups, list):
            raise Undetermined("%s: hooks.%s is not a list" % (path, event))
        for g in groups:
            if not isinstance(g, dict) or not isinstance(g.get("hooks"), list):
                raise Undetermined("%s: hooks.%s has an entry without a `hooks` list" % (path, event))
            for h in g["hooks"]:
                if not isinstance(h, dict):
                    raise Undetermined("%s: hooks.%s has a non-object hook" % (path, event))
                if h.get("type") != "command":
                    continue  # prompt / agent hooks run no script
                cmd = h.get("command")
                if not isinstance(cmd, str):
                    raise Undetermined("%s: hooks.%s has a command hook without a string `command`" % (path, event))
                out.append((event, cmd))
    return out


def follow_chain(scripts_dir, roots):
    """Close `roots` (script names) over the scripts they really invoke.

    A reached `.sh` is read with the shell parser (invoked_by, any_script);
    a reached `.py` with python_invokes(). Only regular files directly in
    scripts_dir are followed; a name that does not exist there reaches
    nothing. A reached file that cannot be read or parsed is undetermined.
    """
    seen = set()
    todo = list(roots)
    while todo:
        name = todo.pop()
        if name in seen:
            continue
        seen.add(name)
        path = os.path.join(scripts_dir, name)
        if not os.path.isfile(path):
            continue
        try:
            with open(path, encoding="utf-8", errors="replace") as fh:
                text = fh.read()
        except OSError as e:
            raise Undetermined("cannot read chained script %s: %s" % (path, e))
        try:
            if name.endswith(".py"):
                nxt = python_invokes(text, path)
            else:
                nxt = invoked_by(text, any_script=True)
        except Undetermined as e:
            raise Undetermined("cannot parse chained script %s: %s" % (path, e))
        todo.extend(nxt - seen)
    return seen


def gate_scanners(scripts_dir):
    try:
        entries = list(os.scandir(scripts_dir))
    except OSError as e:
        raise Undetermined("cannot list %s: %s" % (scripts_dir, e))
    out = set()
    for e in entries:
        if not SCANNER_RE.match(e.name):
            continue
        try:
            if e.is_file():
                out.add(e.name)
        except OSError as err:
            raise Undetermined("cannot stat %s: %s" % (e.path, err))
    return out


USAGE = "usage: %s [--repo PATH] [--hooks-dir DIR]" % NAME


def parse_args(argv):
    """Return (repo, hooks_dir); either may be None. Raise ValueError on misuse.

    Each option takes exactly one value and may be given at most once; any
    other argument is a usage error (exit 2, never a verdict).
    """
    opts = {"--repo": None, "--hooks-dir": None}
    i = 0
    while i < len(argv):
        flag = argv[i]
        if flag not in opts or opts[flag] is not None or i + 1 >= len(argv):
            raise ValueError(flag)
        val = argv[i + 1]
        if not val:
            raise ValueError(flag)
        opts[flag] = val
        i += 2
    return opts["--repo"], opts["--hooks-dir"]


def explicit_hook_dir(cwd, hooks_dir):
    """Resolve --hooks-dir: toplevel from cwd, hook dir as given (no hooksPath).

    A relative DIR is resolved against the invoking process's cwd (like any
    other command-line path), not against --repo. A DIR that does not exist or
    is not a directory is undetermined, never an empty (clean) hook set.
    """
    top = git(cwd, "rev-parse", "--show-toplevel")
    if top.returncode != 0 or not top.stdout.strip():
        raise Undetermined(
            "git rev-parse --show-toplevel exited %d (not a work tree?): %s"
            % (top.returncode, top.stderr.strip())
        )
    hook_dir = os.path.abspath(hooks_dir)
    try:
        st = os.stat(hook_dir)
    except OSError as e:
        raise Undetermined("--hooks-dir %s cannot be stat'ed: %s" % (hook_dir, e))
    if not stat.S_ISDIR(st.st_mode):
        raise Undetermined("--hooks-dir %s is not a directory" % hook_dir)
    return top.stdout.strip(), os.path.normpath(hook_dir), "--hooks-dir %s" % hooks_dir


def main(argv):
    try:
        repo, hooks_dir = parse_args(argv)
    except ValueError:
        print(USAGE, file=sys.stderr)
        return 2
    cwd = repo if repo is not None else os.getcwd()
    try:
        if hooks_dir is not None:
            toplevel, hook_dir, how = explicit_hook_dir(cwd, hooks_dir)
        else:
            toplevel, hook_dir, how = resolve_hook_dir(cwd)
        bodies = read_hooks(hook_dir)
        roots = set()
        for hook_name, text in sorted(bodies.items()):
            try:
                roots |= invoked_by(text, any_script=True)
            except Undetermined as e:
                raise Undetermined("cannot parse hook %s: %s" % (os.path.join(hook_dir, hook_name), e))
        if not any(SCANNER_RE.match(n) for n in roots):
            raise Undetermined(
                "parsed zero scanner invocations from the active hooks in %s (%s); "
                "an empty set is not a clean set" % (hook_dir, ", ".join(sorted(bodies)))
            )
        claude_cmds = claude_hook_commands(toplevel)
        for event, cmd in claude_cmds:
            try:
                roots |= invoked_by(cmd, any_script=True)
            except Undetermined as e:
                raise Undetermined(
                    "cannot parse .claude/settings.json %s hook command %r: %s" % (event, cmd, e)
                )
        scripts_dir = os.path.join(toplevel, "scripts")
        reached = follow_chain(scripts_dir, roots)
        invoked = {n for n in reached if SCANNER_RE.match(n)}
        scanners = gate_scanners(scripts_dir)
        if not scanners:
            raise Undetermined(
                "found zero check-*.py / check-*.sh scanners in %s/scripts"
                % toplevel
            )
    except Undetermined as e:
        print("%s: UNDETERMINED — %s" % (NAME, e), file=sys.stderr)
        return 2

    unlisted = sorted(scanners - invoked)
    if unlisted:
        print(
            "%s: %d gate scanner(s) in %s/scripts are NOT invoked by any active hook "
            "(hook dir %s, from %s; hooks read: %s; .claude/settings.json command hooks: %d):"
            % (NAME, len(unlisted), toplevel, hook_dir, how, ", ".join(sorted(bodies)), len(claude_cmds)),
            file=sys.stderr,
        )
        for n in unlisted:
            print("  scripts/%s" % n, file=sys.stderr)
        print(
            "A scanner the active hooks never run is a gate that blocks nothing (dark,\n"
            "not red). Wire it into the hook git actually executes (merge the hook\n"
            "change to the tree core.hooksPath points at), or remove it. Backlog 666b6704.",
            file=sys.stderr,
        )
        return 1
    print(
        "%s: OK — all %d gate scanner(s) in scripts/ are invoked by the active hooks (%s)"
        % (NAME, len(scanners), hook_dir)
    )
    return 0


if __name__ == "__main__":
    sys.exit(main(sys.argv[1:]))
