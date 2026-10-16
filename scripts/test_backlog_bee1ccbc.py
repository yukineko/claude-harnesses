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

    # Inverted (not deleted) by user ruling 2026-10-14: a missing / non-string
    # command is NOT an unreadable payload and stays exit 0, matching
    # test_gate_bypass.py::test_non_string_command_is_allowed.
    def test_command_missing_is_allowed(self):
        r = run_hook(bash({}))
        self.assertEqual(r.returncode, 0, r.stderr.decode())

    def test_command_not_string_is_allowed(self):
        r = run_hook(bash({"command": ["git", "commit", "--no-verify"]}))
        self.assertEqual(r.returncode, 0, r.stderr.decode())


class PathologicalPayloadRefused(unittest.TestCase):
    """A payload the hook cannot decode must be refused with exit 2 exactly.

    A deeply nested value makes json decoding raise RecursionError; if that
    escapes as an uncaught exception the hook exits 1, which Claude Code
    treats as a non-blocking error = ALLOW, hiding the real --no-verify in
    `command`. `!= 0` would accept that crash, so assert `== 2`.
    """

    def assert_refused(self, stdin: str, label: str):
        r = run_hook(stdin)
        self.assertEqual(
            r.returncode, 2,
            "%s: exit %d (0/1 are both ALLOW to Claude Code); stderr tail=%s"
            % (label, r.returncode, r.stderr.decode()[-500:]),
        )

    def test_deeply_nested_payload_refused(self):
        depth = 100000
        stdin = (
            '{"tool_name":"Bash","tool_input":{"command":"git commit --no-verify -m x","pad":'
            + "[" * depth
            + "]" * depth
            + "}}"
        )
        self.assert_refused(stdin, "deeply nested payload (depth %d)" % depth)

    def test_tool_input_null_refused(self):
        self.assert_refused(
            json.dumps({"tool_name": "Bash", "tool_input": None}),
            "tool_input null",
        )


if __name__ == "__main__":
    unittest.main()
