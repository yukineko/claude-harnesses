#!/usr/bin/env python3
"""RED (false positive, observed 2026-11-20): scripts/deny-no-verify.py refuses a
command whose only "unbalanced quote" is an apostrophe inside a QUOTED-delimiter
heredoc body.

Observed: the main session re-ran (byte for byte, relay bgr-a866a4fd1397) a
command that writes a Rust test file from `{ cat <<'EOF' ... EOF; sed -n ...;
... } > $F && grep -n ... $F`, and deny-no-verify.py refused it with

    Refused: this command could not be parsed, and it carries a gate-bypass marker.

    The shell quoting does not close, so whether this is `git commit --no-verify`
    cannot be determined. ...

Mechanism (read from the source, then observed by the tests below):
`split_commands` lexes with `shlex`, which does not model heredocs, so the
apostrophe in body text such as `task's` opens a quote that never closes and
`split_commands` returns None. `looks_like_bypass` then fires because the text
holds the substring `git` and a short-option cluster containing `n` (`sed -n` /
`grep -n`). Neither is a git invocation. In the minimal case `git` comes from
`harness_core::git_env` in the body; in the observed command it came from the
last heredoc body (`git(&wt, ...)` helper calls). The blastguard fixture
abbreviates that body and so carries no `git` at all: run as-is it was allowed
(observed 2026-11-20, XPASS), so the observed test restores one omitted body
line verbatim (see OMITTED_BODY_LINE). For
a real shell the command tokenizes fine: a `<<'EOF'` body is data and its quotes
are not shell quotes.

Relation to the pinned
test_gate_bypass.py::KnownDefectsDenyNoVerify::test_DEFECT_looks_like_bypass_is_noisy_on_untokenizable_input:
that test accepts over-matching on input that really does not lex, where "the
remedy is to balance the quote". Here the quote IS balanced for the shell and
cannot be balanced by the author without editing the data the heredoc writes,
so this case is not covered by that acceptance. That pinned test is not changed.

Asserted (RED, xfail strict): the minimal shape and the observed command are
allowed (exit 0). Controls (pass at HEAD): the same minimal shape without the
apostrophe is allowed (so the RED is about the apostrophe, not the shape); a
real `git commit --no-verify` after a heredoc whose body holds an apostrophe is
refused (exit 2), so a fix may not allow by dropping the screen.

DOES NOT PROVE: unquoted-delimiter heredocs (`<<EOF`, whose body is subject to
expansion); `<<-` tab stripping; heredocs feeding an interpreter (`sh <<'EOF'`,
an open hole by the module docstring).

RED written by an independent test author, not the implementer.

    python3 -m pytest --runxfail scripts/test_deny_no_verify_quoted_heredoc_apostrophe_fp_20261120.py
"""

from __future__ import annotations

import json
import subprocess
import sys
import unittest
from pathlib import Path

import pytest

HERE = Path(__file__).resolve().parent
HOOK = HERE / "deny-no-verify.py"
OBSERVED = (
    HERE.parent
    / "crates/blastguard/tests/fixtures/backlog_20261120_quoted_heredoc_group_backtick_flag_fp.cmd"
)

RED_REASON = (
    "backlog RED: deny-no-verify.py reads an apostrophe inside a quoted heredoc "
    "body as an unclosed shell quote and refuses on sed -n / grep -n + 'git' substring"
)

MINIMAL = (
    "F=out.rs && {\n"
    "cat <<'EOF'\n"
    "//! only the task's own tests may run\n"
    "let out = harness_core::git_env::scrub(&mut cmd);\n"
    "EOF\n"
    "} > $F && grep -n '^fn ' $F"
)


# The blastguard fixture abbreviates the observed command's LAST heredoc body to
# its first line, which drops every `git` substring the observed command had
# (measured: `grep -i git` on the fixture finds nothing), and without `git`
# looks_like_bypass is False, so the hook allows. This restores one line of the
# omitted body verbatim, the fixture helper call from the observed command.
OMITTED_BODY_LINE = '    let branch = git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]);\n'
ABBREVIATED_TAIL = 'const SESSION: &str = "sess-r";\nEOF\n'


def observed_command() -> str:
    text = OBSERVED.read_text().replace("@WT@", "/tmp/wt")
    assert ABBREVIATED_TAIL in text, "fixture tail changed; re-anchor OMITTED_BODY_LINE"
    return text.replace(
        ABBREVIATED_TAIL,
        'const SESSION: &str = "sess-r";\n' + OMITTED_BODY_LINE + "EOF\n",
    )


def run(command: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(HOOK)],
        input=json.dumps({"tool_name": "Bash", "tool_input": {"command": command}}),
        capture_output=True,
        text=True,
        timeout=30,
    )


class QuotedHeredocApostrophe(unittest.TestCase):
    @pytest.mark.xfail(strict=True, reason=RED_REASON)
    def test_minimal_quoted_heredoc_apostrophe_is_allowed(self):
        proc = run(MINIMAL)
        self.assertEqual(proc.returncode, 0, "must ALLOW: %r\n%s" % (MINIMAL, proc.stderr))

    @pytest.mark.xfail(strict=True, reason=RED_REASON)
    def test_observed_command_is_allowed(self):
        command = observed_command()
        proc = run(command)
        self.assertEqual(proc.returncode, 0, "must ALLOW the observed command\n%s" % proc.stderr)

    def test_control_same_shape_without_apostrophe_is_allowed(self):
        command = MINIMAL.replace("task's", "tasks")
        proc = run(command)
        self.assertEqual(proc.returncode, 0, "must ALLOW: %r\n%s" % (command, proc.stderr))

    def test_control_real_bypass_after_apostrophe_heredoc_is_refused(self):
        command = "cat <<'EOF'\nthe task's notes\nEOF\ngit commit --no-verify -m x"
        proc = run(command)
        self.assertEqual(proc.returncode, 2, "must REFUSE: %r\n%s" % (command, proc.stderr))
        self.assertIn("Refused", proc.stderr)


if __name__ == "__main__":
    unittest.main()
