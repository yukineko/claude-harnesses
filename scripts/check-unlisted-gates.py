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
`pre-commit` in that directory must exist, be readable and be executable (git
skips a non-executable hook), else the verdict is undetermined. Every OTHER
standard git hook name present there (pre-push, commit-msg, ...) is read too, so
a scanner that legitimately runs only at push or commit-msg time counts as
listed. A sibling hook that exists but cannot be read is undetermined; one
without the exec bit is skipped, because git never runs it.

Naming rule for "gate scanner"
------------------------------
A regular file directly in `<toplevel>/scripts/` whose name matches
`check-*.py` or `check-*.sh`:
  * every scanner the hooks call today is `scripts/check-*.py` (pre-commit's
    `run` helper hard-codes `$REPO/scripts/$scanner`; commit-msg and pre-push
    name `scripts/check-*.py` directly);
  * `.sh` is included because `check-unwind.sh` and `check-versions.sh` exist
    under the same prefix; excluding them would let a shell scanner stay dark;
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
Anything this parser does not recognise is NOT counted, so an unrecognised
invocation style reads as unlisted (red), never as listed.

KNOWN LIMITATION (fail-open, not fixed): reachability is not analysed. A
recognised invocation is counted even if it can never execute — inside a
function that is defined but never called, under `if false`, after an
unconditional `exit 0`, behind a `case` arm no value reaches, and so on. Such a
scanner reads as listed while no commit ever runs it. Closing this would need
control-flow analysis of the hook; it is left open on purpose and stated here
rather than implied away.

Exit codes
----------
  0  every gate scanner in scripts/ is invoked; prints one OK line with the count
  1  one or more scanners are not invoked by any active hook; each is named
  2  undetermined: not a git work tree, hooksPath unreadable, active pre-commit
     missing/unreadable/not executable, a sibling hook unreadable, a hook body
     that cannot be parsed (unbalanced quoting / nesting, unterminated or
     unparseable heredoc), scripts/
     unreadable or missing, zero gate scanners found, or zero invocations parsed
     from the hooks (an empty set is not a clean set).
"""

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
    """
    src = body.replace("\\\n", "  ")
    n = len(src)
    stack = ["top"]  # top | paren | cmdsub | bq | dq | sq
    starts = []
    cmd_pos = True
    assign_depth = None  # stack depth of a leading VAR=value word being walked
    heredocs = []
    i = 0
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
                cmd_pos = True
                assign_depth = None
                i += 2
            elif c == "`":
                stack.append("bq")
                cmd_pos = True
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
            cmd_pos = True
            assign_depth = None
            i += 2
            continue
        if c == "`":
            stack.append("bq")
            cmd_pos = True
            assign_depth = None
            i += 1
            continue
        if c == "(":
            stack.append("paren")
            cmd_pos = True
            assign_depth = None
            i += 1
            continue
        if c == ")":
            if ctx in ("paren", "cmdsub"):
                stack.pop()
                cmd_pos = False
            else:
                # Unmatched at this level: a `case` pattern terminator.
                cmd_pos = True
            assign_depth = None
            i += 1
            continue
        if c in ";&|{}":
            cmd_pos = True
            assign_depth = None
            i += 1
            continue
        if cmd_pos:
            starts.append(i)
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
    return src, starts


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


def _script_name(operand):
    m = SCRIPT_PATH_RE.match(operand)
    return m.group(1) if m else None


def invoked_by(text):
    """Return the set of check-* scanner names a hook body invokes.

    Raises Undetermined when the body cannot be parsed (see command_starts).
    """
    src, starts = command_starts(text)
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
            name = _script_name(val)
            if name:
                bindings[asg.group(1)] = name
            continue
        m = RUN_RE.match(src, i)
        if m:
            run_calls.add(m.group(1))
            continue
        m = INTERP_RE.match(src, i)
        if m:
            operand = m.group(2)
            name = _script_name(operand)
            if name:
                found.add(name)
                continue
            v = VAR_REF_RE.match(operand)
            if v:
                refs.add(v.group(1))
            continue
        m = DIRECT_RE.match(src, i)
        if m:
            name = _script_name(m.group(2))
            if name:
                found.add(name)
    if has_run:
        found |= run_calls
    for var in refs:
        if var in bindings:
            found.add(bindings[var])
    return found


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


def main(argv):
    cwd = os.getcwd()
    if len(argv) == 2 and argv[0] == "--repo":
        cwd = argv[1]
    elif argv:
        print("usage: %s [--repo PATH]" % NAME, file=sys.stderr)
        return 2
    try:
        toplevel, hook_dir, how = resolve_hook_dir(cwd)
        bodies = read_hooks(hook_dir)
        invoked = set()
        for hook_name, text in sorted(bodies.items()):
            try:
                invoked |= invoked_by(text)
            except Undetermined as e:
                raise Undetermined("cannot parse hook %s: %s" % (os.path.join(hook_dir, hook_name), e))
        if not invoked:
            raise Undetermined(
                "parsed zero scanner invocations from the active hooks in %s (%s); "
                "an empty set is not a clean set" % (hook_dir, ", ".join(sorted(bodies)))
            )
        scanners = gate_scanners(os.path.join(toplevel, "scripts"))
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
            "(hook dir %s, from %s; hooks read: %s):"
            % (NAME, len(unlisted), toplevel, hook_dir, how, ", ".join(sorted(bodies))),
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
