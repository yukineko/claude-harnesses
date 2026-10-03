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
Parsed from shell text, outside comments, heredoc bodies and quoted string
literals (so a remediation hint that merely *mentions* `python3 scripts/x.py`
does not list x), at a command position (line start, after ; & | ( ) { } `$(`
or a backtick, after `then/do/else/if/...`, after leading VAR=value words):
  1. `run <check-name>` — only in a hook that defines a `run()` function (the
     pre-commit helper);
  2. `python3|python|bash|sh [-opts] <...>scripts/<check-name>`;
  3. a direct exec of `<...>scripts/<check-name>`;
  4. `VAR=<...>scripts/<check-name>` followed elsewhere in the same hook by
     `python3|python|bash|sh "$VAR"` (commit-msg's SCANNER= form).
Anything this parser does not recognise is NOT counted, so an unrecognised
invocation style reads as unlisted (red), never as listed.

Exit codes
----------
  0  every gate scanner in scripts/ is invoked; prints one OK line with the count
  1  one or more scanners are not invoked by any active hook; each is named
  2  undetermined: not a git work tree, hooksPath unreadable, active pre-commit
     missing/unreadable/not executable, a sibling hook unreadable, scripts/
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

INTERP = r"(?:exec\s+|command\s+)?(?:python3|python|bash|sh)\s+(?:-[A-Za-z]+\s+)*"
# An operand word: optionally quoted, no whitespace / shell metachar inside.
OPERAND = r"([\"']?)([^\s\"';|&()<>`]+)\1"
SCRIPT_PATH_RE = re.compile(r"^(?:\$\{?\w+\}?/|\./|[^\s\"']*/)?scripts/(" + CHECK_NAME + r")$")
VAR_REF_RE = re.compile(r"^\$\{?(\w+)\}?$")

INTERP_RE = re.compile(INTERP + OPERAND)
DIRECT_RE = re.compile(OPERAND)
RUN_RE = re.compile(r"run\s+(" + CHECK_NAME + r")(?=[\s;&|)]|$)")
ASSIGN_RE = re.compile(r"([A-Za-z_]\w*)=")
KEYWORD_RE = re.compile(r"(?:then|do|else|elif|if|while|until|time|!)(?=\s)")
HEREDOC_RE = re.compile(r"<<-?\s*([\"']?)([A-Za-z_]\w*)\1")
RUN_DEF_RE = re.compile(r"^\s*(?:function\s+run\b|run\s*\(\s*\))", re.M)


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


def logical_lines(text):
    """Yield shell lines with heredoc bodies removed and continuations joined."""
    lines = text.split("\n")
    i = 0
    buf = ""
    while i < len(lines):
        line = lines[i]
        i += 1
        if line.endswith("\\") and not line.endswith("\\\\"):
            buf += line[:-1] + " "
            continue
        logical = buf + line
        buf = ""
        yield logical
        # Heredoc bodies are data (remediation text etc.), never commands.
        for delim in _heredoc_delims(logical):
            while i < len(lines) and lines[i].strip() != delim:
                i += 1
            i += 1  # the delimiter line
    if buf:
        yield buf


def _heredoc_delims(line):
    out = []
    for kind, pos, _ in _unquoted_positions(line):
        if kind == "<<":
            m = HEREDOC_RE.match(line, pos)
            if m:
                out.append(m.group(2))
    return out


def _unquoted_positions(line):
    """Yield (kind, index, cmd_pos) markers by walking the line quote-aware.

    kind is "cmd" for a command-position start, "<<" for an unquoted heredoc
    operator. Stops at an unquoted comment.
    """
    in_single = False
    in_double = False
    cmd_pos = True
    assign_word = False  # inside a leading VAR=value word
    i = 0
    n = len(line)
    while i < n:
        c = line[i]
        if in_single:
            if c == "'":
                in_single = False
            i += 1
            continue
        if c == "\\":
            i += 2
            cmd_pos = False
            continue
        if c in " \t":
            if assign_word and not in_double:
                # `VAR=value cmd`: the word after an assignment is a command.
                assign_word = False
                cmd_pos = True
            i += 1
            continue
        if line.startswith("$(", i) and not line.startswith("$((", i):
            i += 2
            cmd_pos = True
            continue
        if c == "`":
            i += 1
            cmd_pos = True
            continue
        if in_double and (c == '"' or not cmd_pos):
            # Inside "...": only a `$(`/backtick (handled above) starts a
            # command; everything else is string data.
            if c == '"':
                in_double = False
            cmd_pos = False
            i += 1
            continue
        if c == "#" and (i == 0 or line[i - 1] in " \t;&|()"):
            return
        if line.startswith("<<", i) and not line.startswith("<<<", i):
            yield ("<<", i, False)
            i += 2
            cmd_pos = False
            continue
        if c in ";&|(){}":
            i += 1
            cmd_pos = True
            assign_word = False
            continue
        if cmd_pos:
            yield ("cmd", i, True)
            kw = KEYWORD_RE.match(line, i)
            if kw:
                i = kw.end()
                continue
            asg = ASSIGN_RE.match(line, i)
            if asg:
                # Walk INTO the value: `x="$(python3 scripts/y.py)"` runs y.
                i = asg.end()
                cmd_pos = False
                assign_word = True
                continue
            cmd_pos = False
        if c == '"':
            in_double = True
        elif c == "'":
            in_single = True
        i += 1


def _skip_word(line, i):
    """Skip one shell word starting at i (quote-aware); return the index after."""
    n = len(line)
    in_single = in_double = False
    depth = 0
    while i < n:
        c = line[i]
        if in_single:
            in_single = c != "'"
        elif c == "\\":
            i += 1
        elif line.startswith("$(", i):
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
        elif c in " \t;&|" and not depth:
            break
        i += 1
    return i


def _script_name(operand):
    m = SCRIPT_PATH_RE.match(operand)
    return m.group(1) if m else None


def invoked_by(text):
    """Return the set of check-* scanner names a hook body invokes."""
    has_run = bool(RUN_DEF_RE.search(text))
    found = set()
    bindings = {}
    refs = set()
    for line in logical_lines(text):
        for kind, i, _ in _unquoted_positions(line):
            if kind != "cmd":
                continue
            asg = ASSIGN_RE.match(line, i)
            if asg:
                val = line[asg.end():_skip_word(line, asg.end())].strip("\"'")
                name = _script_name(val)
                if name:
                    bindings[asg.group(1)] = name
                continue
            if has_run:
                m = RUN_RE.match(line, i)
                if m:
                    found.add(m.group(1))
                    continue
            m = INTERP_RE.match(line, i)
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
            m = DIRECT_RE.match(line, i)
            if m:
                name = _script_name(m.group(2))
                if name:
                    found.add(name)
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
        for text in bodies.values():
            invoked |= invoked_by(text)
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
