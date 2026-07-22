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
side" being waived — this hook is not the gate. The gate is `.githooks/*` plus
the bypass ledger, which run regardless of what happens here. Blocking every
Bash call in the session because one payload failed to decode would take the
whole turn down to protect a check that has a working backstop. The cost of that
asymmetry is stated here rather than left for a reader to discover.

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
resolves to DENY rather than allow whenever the raw text carries the markers a
bypass necessarily has (`--no-verify`, or `-n` alongside `git`). That screen is
a necessary condition, not a guess: a command whose raw text contains neither
marker cannot be one of the two bypasses this hook refuses, so allowing it is a
decision, not a shrug. The payload branch above keeps its exemption; this one
does not get to borrow it.
"""

from __future__ import annotations

import json
import re
import shlex
import sys

# `git commit -n` is the short form of --no-verify. `-n` means something else to
# other git subcommands entirely (`git log -n 5`), so the short form is only
# treated as a bypass for the two subcommands that honour it.
BYPASS_LONG = "--no-verify"
BYPASS_SHORT = "-n"
GUARDED_SUBCOMMANDS = ("commit", "push", "merge")

# git's global options that take their value as a SEPARATE token. The value is
# not a flag, so it would otherwise be mistaken for the subcommand.
GLOBAL_OPTS_WITH_VALUE = ("-C", "-c", "--git-dir", "--work-tree", "--namespace",
                          "--exec-path", "--config-env")


# Shell control operators that end one command and start the next. `&` is here
# too: `git commit --no-verify & ` backgrounds it, and a bypass that runs in the
# background is still a bypass.
SEPARATORS = frozenset({";", "&&", "||", "|", "&", "\n", ";;", "|&"})


def split_commands(command: str) -> list[list[str]] | None:
    """Tokenize once, then split on separator TOKENS. None if it will not lex.

    The separator split has to happen after tokenization, not before. Splitting
    the raw string on `;`/`|` cuts through quoted text — see the module
    docstring for the observed bypass that produced. `punctuation_chars` makes
    shlex emit `;`, `|`, `&&` as tokens of their own while leaving a quoted one
    inside its argument, which is exactly the distinction that was missing.

    Returns None when the command does not tokenize at all (an unbalanced
    quote). That is a cannot-determine and the caller must not read it as
    "no bypass here".
    """
    lexer = shlex.shlex(command, posix=True, punctuation_chars=True)
    lexer.whitespace_split = True
    try:
        tokens = list(lexer)
    except ValueError:
        return None

    segments: list[list[str]] = []
    current: list[str] = []
    for tok in tokens:
        if tok in SEPARATORS:
            if current:
                segments.append(current)
            current = []
        else:
            current.append(tok)
    if current:
        segments.append(current)
    return segments


def looks_like_bypass(command: str) -> bool:
    """Necessary condition for the two bypasses this hook refuses.

    Used only when tokenization failed, to decide what a cannot-determine
    resolves to. A raw string carrying neither `--no-verify` nor a `-n` next to
    a `git` cannot be `git commit/push/merge --no-verify` or `git commit -n`,
    so allowing it is a decision rather than a shrug. It over-matches freely —
    over-matching costs a refusal the author can rephrase, under-matching costs
    the whole gate.
    """
    if BYPASS_LONG in command:
        return True
    return "git" in command and re.search(r"(?<!\w)-n(?!\w)", command) is not None


def is_bypass(tokens: list[str]) -> tuple[str, str] | None:
    """Return (subcommand, flag) if these tokens are a gate-skipping git call.

    Returns the subcommand rather than leaving the caller to re-derive it: the
    obvious re-derivation (`tokens.index("git")`) raises on an absolute path
    like `/usr/bin/git`, which turned a denial into a crash — observed, then
    pinned by scripts/test_gate_bypass.py.
    """
    if not tokens:
        return None

    # Skip leading env assignments (`FOO=1 git commit …`).
    i = 0
    while i < len(tokens) and "=" in tokens[i] and not tokens[i].startswith("-"):
        i += 1
    if i >= len(tokens):
        return None
    if tokens[i].split("/")[-1] != "git":
        return None

    rest = tokens[i + 1 :]
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

    for tok in rest:
        if tok == BYPASS_LONG:
            return sub, BYPASS_LONG
        if tok == BYPASS_SHORT and sub in ("commit", "push"):
            return sub, BYPASS_SHORT
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
