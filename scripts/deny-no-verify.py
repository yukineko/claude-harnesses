#!/usr/bin/env python3
"""PreToolUse hook: refuse a Bash command that skips this repo's local gate.

`git commit --no-verify` and `git push --no-verify` cannot be blocked from
inside the hooks they skip — git never runs them. `.githooks/post-commit` makes
the bypass impossible to HIDE (it records the ungated commit, and pre-push then
refuses to send it), but the commit still happens.

For the author that writes most of the code here — an agent issuing Bash calls —
the bypass can be refused outright, because the command passes through this
process before git ever sees it. That is the point of the arrangement: the
authority to stop the workflow stays in this repository's own hands rather than
being delegated to a service, and it is exercised at the last moment we still
control.

A human typing the same command in their own terminal is deliberately NOT
covered. The bypass remains available to a person who decides to take it, and it
stays recorded. Removing a human's ability to override their own tools is not the
goal; removing the SILENCE was.

Protocol: reads the PreToolUse JSON payload on stdin.

    exit 0   allow (also the answer for every non-Bash tool and every command
             this hook does not recognise as a bypass)
    exit 2   deny; stderr is shown to the model as the reason

Fail-closed vs fail-open, deliberately chosen per branch: an UNPARSEABLE payload
exits 0. That is not the doctrine's "cannot determine resolves to the restricted
side" being waived — for the `--no-verify` class this hook is not the gate. The
gate is `.githooks/*` plus the bypass ledger: `post-commit` records a commit this
hook never certified and `pre-push` then refuses to send it. Blocking every Bash
call in the session because one payload failed to decode would take the whole
turn down to protect a check that has a working backstop.

THE BACKSTOP DOES NOT COVER EVERY BYPASS, and an earlier version of this
paragraph claimed it did — "the gate is `.githooks/*` plus the bypass ledger,
which run regardless of what happens here". That is false for `-c
core.hooksPath=`, and the failure was constructed rather than argued:

    git commit --no-verify -m ungated       ledger 1, pre-push exit 1   caught
    git -c core.hooksPath=/dev/null commit  ledger 0, pre-push exit 0   NOT caught

The chain is post-commit -> ledger -> pre-push, and overriding `hooksPath` breaks
it at the first link: `post-commit` never runs, so nothing is recorded, so
`pre-push` reads an empty ledger and lets the ungated commit leave the machine.

So the asymmetry above is narrower than it was written to be. For the hooksPath
class this hook is the ONLY control, not redundant cover, which means a fail-open
here is unconditional. The payload branch keeps its exemption on the reasoning
that a Bash payload which fails to DECODE is a harness fault rather than an
author's command — but it no longer gets to lean on a backstop that, for one of
the two bypass classes it covers, is not there.

An unparseable COMMAND is a different question, and it used to be answered the
same permissive way — wrongly. Splitting on `;` / `|` with a regex BEFORE
tokenizing cut straight through quoted text, so `git commit --no-verify -m "a; b"`
was torn into a fragment with an unbalanced quote, `shlex.split` raised, the
`except ValueError: return None` read as "not a bypass", and the hook exited 0.
Observed, not theorised:

    $ printf '%s' '{"tool_name":"Bash","tool_input":{"command":
      "git commit --no-verify -m \\"a; b\\""}}' | python3 scripts/deny-no-verify.py
    exit=0            # …while the same command without the semicolon exits 2

A single character in a commit message turned the refusal off. The separator
split is now done on TOKENS (`shlex` with `punctuation_chars`), so a quoted
separator stays inside its argument, and a command that still will not tokenize
resolves to DENY rather than allow whenever the raw text carries a bypass
marker. The payload branch above keeps its exemption; this one does not get to
borrow it.

WHAT THIS HOOK DOES NOT CATCH. An adversarial verifier reproduced the following
against real git, and they are recorded here rather than left for the next
reader to rediscover:

  - The first fix REGRESSED multi-line commands. `punctuation_chars` does not
    make shlex emit a newline — it is whitespace — so `cargo test\ngit commit
    --no-verify` fused into one segment and was allowed, while the pre-fix regex
    had caught it. Fixed by moving `\n` out of the lexer's whitespace set; the
    reasoning is in `split_commands`. A gate that was verified only against the
    bug it targeted let a wider one in.

  - FIXED (backlog 2cef09c5): interpreter wrappers (`sh -c '…'`, `bash -lc`,
    `eval`), command prefixes that shift `git` out of argv[0] (`env`, `nohup`,
    `time`, `xargs`, `nice`, `timeout`, `sudo`, …) and the shell keywords that
    open a compound form (`if … then`, `for … do`, `while`, `{`, `!`). These were
    all confirmed to reach git while `is_bypass` inspected only argv[0]. Now a
    `-c` script / `eval` argument is re-split and judged recursively like a
    top-level command, a leading keyword is dropped, and after a prefix command
    EVERY later position is tried as the start of a command (over-matching,
    e.g. `time echo git commit --no-verify`, is the cheap direction). Nesting
    deeper than MAX_NESTING, or a nested script that will not tokenize, resolves
    to DENY when it carries a bypass marker, like the top level.

  - STILL OPEN: command substitution (`$(git commit --no-verify)` or backticks
    inside another command's argument), an interpreter reading its script from
    a FILE or stdin (`bash script.sh`, `echo … | sh`), `env -S '…'`, and any
    prefix program not in PREFIX_COMMANDS. These are known holes, not clean
    paths.

This hook is therefore a REDUCTION in the ways the gate can be stepped around,
not a proof that it cannot be. The claim that the marker screen was "a necessary
condition" appeared here and was false; `-nm`, `--no-verif`, and
`-c core.hooksPath=` were all bypasses carrying none of the old markers.
"""

from __future__ import annotations

import json
import re
import shlex
import sys

# `git commit -n` is the short form of --no-verify. `-n` means something else to
# other git subcommands entirely, so the short form is only treated as a bypass
# for the subcommand that actually honours it. Checked against the git on this
# machine rather than assumed:
#
#     git commit -h  ->  -n, --no-verify   bypass pre-commit and commit-msg hooks
#     git push   -h  ->  -n, --dry-run     dry run
#     git merge  -h  ->      --no-verify   (no short form at all)
#
# So `push` and `merge` are guarded for the LONG flag only. An earlier version of
# this file claimed `-n` was honoured by "the two subcommands" and refused
# `git push -n origin main` — a dry run, the safest command in the list. The test
# that pinned that belief was asserting a falsehood as spec; both are corrected.
BYPASS_LONG = "--no-verify"
SHORT_FLAG_SUBCOMMANDS = ("commit",)
GUARDED_SUBCOMMANDS = ("commit", "push", "merge")

# git accepts any UNAMBIGUOUS abbreviation of a long option, so `--no-verif`
# bypasses just as well as the full spelling. Matching the literal string let
# every abbreviation through. Shorter than `--no-v` is ambiguous and git itself
# rejects it (exit 129), but over-matching here costs nothing: the author
# rephrases, and a refusal is the cheap direction.
BYPASS_LONG_MIN = "--no-v"

# A short-option CLUSTER containing `n` (`-nm`, `-anm`, `-nqm`) is `--no-verify`
# just as much as a bare `-n` is; matching only the whole token `-n` missed all
# of them. Clusters are letters only — `-n5` is a value, not a cluster.
SHORT_CLUSTER = re.compile(r"^-[A-Za-z]*n[A-Za-z]*$")

# Subcommand options whose value is a MANDATORY SEPARATE token. Their value must
# not be read as a flag: `git commit -m -n` is a commit whose message is "-n".
#
# Membership is decided by the form git prints in `git commit -h`, and the
# distinction is load-bearing rather than pedantic:
#
#     -F, --file <file>          MANDATORY separate token   -> belongs here
#     -S, --gpg-sign[=<key-id>]  OPTIONAL, ATTACHED         -> must NOT be here
#
# An earlier version put `-S`/`--gpg-sign` in this list. git does not consume the
# following token for it, but the skip did, so `git commit -S --no-verify` had
# its bypass flag swallowed as if it were a key-id and the hook exited 0 — a
# bypass INTRODUCED by the false-positive fix, refused by every prior version of
# this file. Before adding an option here, check `git commit -h`: `[=<...>]`
# means the value is attached and the option takes no separate token.
# (`-u`/`--untracked-files` is the other `[=...]` form; likewise excluded.)
OPTS_WITH_VALUE = ("-m", "--message", "-F", "--file", "-C", "--reuse-message",
                   "-c", "--reedit-message", "--author", "--date",
                   "-t", "--template", "--fixup", "--squash",
                   "--cleanup", "--trailer", "--pathspec-from-file")

# git's global options that take their value as a SEPARATE token. The value is
# not a flag, so it would otherwise be mistaken for the subcommand.
GLOBAL_OPTS_WITH_VALUE = ("-C", "-c", "--git-dir", "--work-tree", "--namespace",
                          "--exec-path", "--config-env")


# Shell control operators that end one command and start the next. `&` is here
# too: `git commit --no-verify & ` backgrounds it, and a bypass that runs in the
# background is still a bypass. `(` and `)` are here because a subshell shifts
# `git` out of argv[0] — `( git commit --no-verify )` was reaching git while
# this hook read `(` as the program name.
SEPARATORS = frozenset({";", "&&", "||", "|", "&", ";;", "|&", "(", ")"})

# Newlines are separators too, but they arrive as their own token shape (`\n`,
# `\r\n`) rather than as one of the operators above.
NEWLINE_TOKEN = re.compile(r"^[\r\n]+$")

# Passing `-c core.hooksPath=...` disables the hooks outright, so it is a gate
# bypass that carries neither `--no-verify` nor `-n`.
HOOKSPATH_OVERRIDE = "core.hookspath="


def split_commands(command: str) -> list[list[str]] | None:
    """Tokenize once, then split on separator TOKENS. None if it will not lex.

    The separator split has to happen after tokenization, not before. Splitting
    the raw string on `;`/`|` cuts through quoted text — see the module
    docstring for the observed bypass that produced. `punctuation_chars` makes
    shlex emit `;`, `|`, `&&` as tokens of their own while leaving a quoted one
    inside its argument, which is exactly the distinction that was missing.

    A newline has to be taken away from shlex's `whitespace` set and handed to
    `punctuation_chars` instead, or it is swallowed as ordinary spacing and the
    commands either side of it FUSE into one segment. That is not hypothetical:
    the first version of this function had `"\\n"` in SEPARATORS but a lexer that
    could never emit it, so

        cargo test\\ngit commit --no-verify -m x

    lexed to a single segment whose argv[0] was `cargo`, and the hook allowed it
    — a regression wider than the semicolon bug it was written to fix, because a
    multi-line command is the ordinary shape, not an edge case. Found by an
    adversarial verifier, not by this author.

    Returns None when the command does not tokenize at all (an unbalanced
    quote). That is a cannot-determine and the caller must not read it as
    "no bypass here".
    """
    # punctuation_chars has no setter, so the newline additions go here.
    lexer = shlex.shlex(command, posix=True, punctuation_chars="();<>|&\n\r")
    lexer.whitespace_split = True
    lexer.whitespace = lexer.whitespace.replace("\n", "").replace("\r", "")
    try:
        tokens = list(lexer)
    except ValueError:
        return None

    segments: list[list[str]] = []
    current: list[str] = []
    for tok in tokens:
        if tok in SEPARATORS or NEWLINE_TOKEN.match(tok):
            if current:
                segments.append(current)
            current = []
        else:
            # A backslash line continuation is an ESCAPE to posix shlex, not a
            # separator, so the newline survives GLUED to the front of the next
            # token: `git commit \<nl>--no-verify` lexes to
            # ['git', 'commit', '\n--no-verify']. The flag then matches nothing,
            # and a continuation before the subcommand yields '\ncommit', which
            # is not in GUARDED_SUBCOMMANDS, so the segment is dismissed before
            # the flag is even looked at. Both forms reached real git with the
            # hook silent, in every version of this file including the first.
            # The shell removes `\<nl>` entirely; stripping it here is the same
            # answer arrived at one step later.
            current.append(tok.lstrip("\r\n"))
    if current:
        segments.append(current)
    return segments


def looks_like_bypass(command: str) -> bool:
    """Necessary condition for the two bypasses this hook refuses.

    Used only when tokenization failed, to decide what a cannot-determine
    resolves to. It over-matches freely — over-matching costs a refusal the
    author can rephrase, under-matching costs the whole gate.

    This is a HEURISTIC, and the distinction matters enough to state plainly:
    an earlier version of this docstring called it "a necessary condition, not a
    guess", claiming a command carrying neither marker "cannot" be a bypass.
    That was false and was demonstrated false — `git commit -nm 'unbalanced`,
    `--no-verif`, and `-c core.hooksPath=/dev/null` are all real bypasses that
    the old screen let through. The markers below are wider now, but a wider
    heuristic is still a heuristic: an untokenizable command that dodges all of
    them is allowed, and that residue is a known, unfixed hole rather than a
    proof of absence.
    """
    lowered = command.lower()
    if BYPASS_LONG_MIN in lowered or HOOKSPATH_OVERRIDE in lowered:
        return True
    # Any short-option cluster containing `n` alongside a `git`.
    return "git" in lowered and re.search(r"(?<![\w-])-[A-Za-z]*n[A-Za-z]*(?![\w-])",
                                          command) is not None


def is_bypass(tokens: list[str], _depth: int = 0) -> tuple[str, str] | None:
    """Return (subcommand, flag) if these tokens are a gate-skipping git call.

    `git` need not be argv[0]: a leading shell keyword, a prefix command
    (PREFIX_COMMANDS), an interpreter `-c` script (INTERPRETERS) or an `eval`
    argument is looked through, recursively (backlog 2cef09c5).

    Returns the subcommand rather than leaving the caller to re-derive it: the
    obvious re-derivation (`tokens.index("git")`) raises on an absolute path
    like `/usr/bin/git`, which turned a denial into a crash — observed, then
    pinned by scripts/test_gate_bypass.py.
    """
    if not tokens:
        return None
    if _depth > MAX_NESTING:
        return _undetermined_nested(" ".join(tokens))

    # Skip leading env assignments (`FOO=1 git commit …`) and shell keywords
    # that open a compound form (`then git commit …`, `do git commit …`).
    i = 0
    while i < len(tokens) and (
        tokens[i] in SHELL_KEYWORDS
        or ("=" in tokens[i] and not tokens[i].startswith("-"))
    ):
        i += 1
    if i >= len(tokens):
        return None
    prog = tokens[i].split("/")[-1]

    if prog in INTERPRETERS:
        return _interpreter_bypass(tokens[i + 1 :], _depth)
    if prog == "eval":
        # eval concatenates its arguments with spaces and runs the result.
        return _nested_bypass(" ".join(tokens[i + 1 :]), _depth)
    if prog in PREFIX_COMMANDS:
        # The prefix's own option grammar (`nice -n 5`, `timeout 5`,
        # `xargs -I{}`, `env -u NAME`) decides where the wrapped command
        # starts, and modelling each one is where a gap would hide. Try EVERY
        # later position instead: a false positive costs a rephrase, a miss
        # costs the gate.
        for j in range(i + 1, len(tokens)):
            hit = is_bypass(tokens[j:], _depth + 1)
            if hit:
                return hit
        return None
    if prog != "git":
        return None

    rest = tokens[i + 1 :]

    # `git -c core.hooksPath=/dev/null commit` needs no --no-verify: it turns the
    # hooks off wholesale, and it does so for `post-commit` too, so the ledger
    # that is supposed to record an ungated commit never runs either. Checked
    # before the subcommand scan because it is a bypass of any subcommand.
    #
    # Only the VALUE of `-c` counts. Scanning every token made this a substring
    # test over the whole command, so `git commit -m 'set core.hooksPath=... to
    # enable'` — prose — was refused.
    for k, tok in enumerate(rest):
        if tok not in ("-c", "--config-env") or k + 1 >= len(rest):
            continue
        if HOOKSPATH_OVERRIDE in rest[k + 1].lower():
            # Deliberately refuses even a value pointing AT the repo's real
            # hooks, which would strengthen the gate rather than skip it.
            # Telling those apart means resolving the path against the repo's
            # own config, and guessing wrong in the permissive direction costs
            # the gate; the author can simply omit an override that is already
            # the configured default.
            return "any subcommand", "-c core.hooksPath"

    sub = None
    j = 0
    while j < len(rest):
        tok = rest[j]
        if tok in GLOBAL_OPTS_WITH_VALUE:
            # `git -C /path commit --no-verify` puts a NON-flag token before the
            # subcommand. Naively taking the first non-flag token reads "/path"
            # as the subcommand, finds it unguarded, and lets the bypass through.
            # Found by review, before any test existed; pinned afterwards.
            j += 2
            continue
        if tok.startswith("-"):
            j += 1
            continue
        sub = tok
        break
    if sub not in GUARDED_SUBCOMMANDS:
        return None

    k = 0
    while k < len(rest):
        tok = rest[k]
        # `--` ends the options; everything after it is a pathspec. A file
        # literally named `-n` is not the bypass flag.
        if tok == "--":
            break
        # Skip the VALUE of an option that takes one, so `git commit -m -n` (a
        # commit whose message is "-n") is not read as the short bypass flag.
        if tok in OPTS_WITH_VALUE:
            k += 2
            continue
        # `--no-verify`, and every abbreviation of it git would accept. The `=`
        # split covers `--no-verify=true`; git rejects that form, but refusing it
        # costs nothing and reading it as "not the flag" would not.
        flag = tok.split("=", 1)[0]
        if flag.startswith(BYPASS_LONG_MIN) and BYPASS_LONG.startswith(flag):
            return sub, tok
        if sub in SHORT_FLAG_SUBCOMMANDS and SHORT_CLUSTER.match(tok):
            return sub, tok
        k += 1
    return None


# Shell reserved words that can precede a command inside a compound form. After
# the separator split, `if true; then git commit --no-verify; fi` yields the
# segment ['then', 'git', ...], whose argv[0] is not a program at all.
SHELL_KEYWORDS = frozenset({"if", "then", "else", "elif", "do", "while", "until",
                            "!", "{", "}"})

# Programs that run their ARGUMENTS as a command. Their own option grammars
# differ, so is_bypass tries every later position rather than parsing them.
PREFIX_COMMANDS = frozenset({"env", "nohup", "time", "command", "builtin", "exec",
                             "nice", "timeout", "gtimeout", "xargs", "sudo", "doas",
                             "stdbuf", "caffeinate", "noglob", "nocorrect",
                             "unbuffer", "setsid", "chronic", "ionice"})

# Shells whose `-c <script>` argument is itself a command line.
INTERPRETERS = frozenset({"sh", "bash", "zsh", "dash", "ksh", "mksh", "ash"})

# Recursion bound for nested wrappers (`bash -c "eval 'sh -c …'"`). Past it the
# nesting is not judged, which is cannot-determine and resolves like the
# top-level untokenizable case: DENY when a bypass marker is present.
MAX_NESTING = 8


def _undetermined_nested(command: str) -> tuple[str, str] | None:
    if looks_like_bypass(command):
        return "<nested command>", "<undeterminable text carrying a bypass marker>"
    return None


def _nested_bypass(command: str, depth: int) -> tuple[str, str] | None:
    """Judge a command string an interpreter or `eval` will run, like a top-level one."""
    if depth >= MAX_NESTING:
        return _undetermined_nested(command)
    segments = split_commands(command)
    if segments is None:
        return _undetermined_nested(command)
    for segment in segments:
        hit = is_bypass(segment, depth + 1)
        if hit:
            return hit
    return None


def _interpreter_bypass(args: list[str], depth: int) -> tuple[str, str] | None:
    """`sh -c SCRIPT`, `bash -lc SCRIPT`, `bash -e -o pipefail -c SCRIPT`.

    The script is the first operand after an option cluster containing `c`.
    Without `-c` the interpreter reads a file or stdin, which this hook cannot
    see (listed as open in the module docstring).
    """
    saw_c = False
    k = 0
    while k < len(args):
        tok = args[k]
        if tok in ("-o", "+o", "-O", "+O"):
            k += 2
            continue
        if tok == "--":
            k += 1
            break
        if tok.startswith("--"):
            k += 1
            continue
        if len(tok) > 1 and tok[0] in "-+":
            if "c" in tok[1:]:
                saw_c = True
            k += 1
            continue
        break
    if saw_c and k < len(args):
        return _nested_bypass(args[k], depth)
    return None


DENIAL = """Refused: `git {sub}` with `{flag}` skips this repository's local gate.

The gate is the enforcement point here — there is no CI required-status-check
standing behind it, by design (CLAUDE.md, 最上位の方針 7). Skipping it does not
make the problem go away; `.githooks/post-commit` records the ungated commit and
`pre-push` then refuses to send it, so the bypass costs more than fixing the
finding.

If a gate is blocking you, the gate found something. Read what it printed and
fix that. If you believe the gate itself is wrong, say so and leave it red —
a wrong gate is a defect worth reporting, not worth stepping around.
"""

UNDETERMINED = """Refused: this command could not be parsed, and it carries a gate-bypass marker.

The shell quoting does not close, so whether this is `git commit --no-verify`
cannot be determined. A check that could not run has not passed, so this
resolves to a refusal rather than to permission.

Rephrase the command so it tokenizes (balance the quotes), and it will be
judged on what it actually says.
"""


def main() -> int:
    try:
        payload = json.load(sys.stdin)
    except (json.JSONDecodeError, UnicodeDecodeError, ValueError):
        return 0  # see module docstring: this hook is not the gate

    if payload.get("tool_name") != "Bash":
        return 0
    command = (payload.get("tool_input") or {}).get("command")
    if not isinstance(command, str):
        return 0

    segments = split_commands(command)
    if segments is None:
        # Cannot determine whether this is a bypass. Refuse only when the raw
        # text carries a marker a bypass necessarily has, so an unbalanced quote
        # in an unrelated command does not take the session down.
        if looks_like_bypass(command):
            sys.stderr.write(UNDETERMINED)
            return 2
        return 0

    for segment in segments:
        hit = is_bypass(segment)
        if hit:
            sub, flag = hit
            sys.stderr.write(DENIAL.format(sub=sub, flag=flag))
            return 2
    return 0


if __name__ == "__main__":
    sys.exit(main())
