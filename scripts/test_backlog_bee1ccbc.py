#!/usr/bin/env python3
"""Repro test for backlog bee1ccbc: scripts/deny-no-verify.py must refuse
(exit 2) a PreToolUse payload it cannot read.

Claude Code treats PreToolUse exit 2 as deny, exit 0 as allow, and any other
exit code (e.g. 1 from an uncaught exception) as a non-blocking error, i.e.
effectively ALLOW. The sibling guards scripts/guard-maintree-bash.py and
scripts/guard-maintree-edit.py refuse a non-JSON / non-object payload with
exit 2. deny-no-verify.py instead exits 0 on non-JSON / empty stdin and
crashes (exit 1) on a non-object payload or a non-object `tool_input`.
Per CLAUDE.md 3 ("cannot determine resolves to block") each of these must be
exit 2 exactly -- `!= 0` would accept the crash, which is an allow.

Well-formed controls (non-Bash tool, benign Bash, a real --no-verify) pin that
the hook still works, so the RED cases cannot pass by the hook refusing
everything.
"""

import json
import subprocess
import sys
import unittest
from pathlib import Path

HOOK = Path(__file__).resolve().parent / "deny-no-verify.py"


def run_hook(stdin: str) -> subprocess.CompletedProcess:
    return subprocess.run(
        [sys.executable, str(HOOK)],
        input=stdin.encode("utf-8"),
        capture_output=True,
        timeout=30,
    )


def bash(tool_input) -> str:
    return json.dumps({"tool_name": "Bash", "tool_input": tool_input})


class WellFormedControls(unittest.TestCase):
    def test_non_bash_tool_allowed(self):
        r = run_hook(json.dumps({"tool_name": "Read", "tool_input": {"file_path": "/x"}}))
        self.assertEqual(r.returncode, 0, r.stderr.decode())

    def test_benign_bash_allowed(self):
        r = run_hook(bash({"command": "ls"}))
        self.assertEqual(r.returncode, 0, r.stderr.decode())

    def test_no_verify_denied(self):
        r = run_hook(bash({"command": "git commit --no-verify -m x"}))
        self.assertEqual(r.returncode, 2, r.stderr.decode())


class UnreadablePayloadRefused(unittest.TestCase):
    def assert_refused(self, stdin: str):
        r = run_hook(stdin)
        self.assertEqual(
            r.returncode, 2,
            "payload %r: exit %d (0/1 are both ALLOW to Claude Code); stderr=%s"
            % (stdin, r.returncode, r.stderr.decode()),
        )

    def test_non_json(self):
        self.assert_refused("not json {")

    def test_empty_stdin(self):
        self.assert_refused("")

    def test_array_payload(self):
        self.assert_refused("[1,2]")

    def test_string_payload(self):
        self.assert_refused('"str"')

    def test_tool_input_not_object(self):
        self.assert_refused(bash([1, 2]))

    def test_tool_input_string(self):
        self.assert_refused(bash("git commit --no-verify -m x"))

    def test_command_missing(self):
        self.assert_refused(bash({}))

    def test_command_not_string(self):
        self.assert_refused(bash({"command": ["git", "commit", "--no-verify"]}))


if __name__ == "__main__":
    unittest.main()
