#!/usr/bin/env python3
"""RED: blastguard reads heredoc data / Python string literals as shell operations.

Observed 2026-11-16 (session 1b9a1e23) and replayed against the blastguard built
from f6a19c390. Three distinct false positives, each reduced from a real
blocked command in the overwatch violations ledger:

  A. `git commit -q -F - <<'EOF' 2>&1` — a QUOTED-delimiter heredoc (no
     expansion, the body is git's stdin) followed by a redirect on the operator
     line. A body line "unknown dirs directly under ~/.claude/plugins/cache/
     yukineko/ ..." is read as the command `unknown` operating on a protected
     path: rule unknown-verb-protected-path (Ask; Deny in a subagent).
     Without the trailing `2>&1` the same commit is allowed (control), so the
     body is normally treated as data — the redirect breaks that.
     (Logged original: `... git commit -q -F - <<'EOF' 2>&1 | tail -30; ...`.)

  B. `python3 - <<'EOF'` whose program is `print('''Unit tests for
     .githooks/pre-push''')`. The Python string literal is read as the shell
     command `print(Unit` on the protected path `.githooks/pre-push)`.
     (Logged original: a Python heredoc whose triple-quoted literal begins
     "Unit tests for .githooks/pre-push's wiring".)

  C. A Python program that READS an existing file A (`open(A)`) and writes a
     NEW file B (`open(B,'w')`). blastguard reports "`,'w')` onto A", i.e. it
     attributes B's write mode to A. Controls: reading A alone and writing B
     alone are both allowed. (Logged original: reading
     ~/.blastguard/relay/<session>.jsonl and writing a scratchpad file was denied
     as interpreter-unrecoverable-write on the relay file.)

Expected (what this test asserts): A and B are allowed, and C gets the same
verdict as "write B only" and does not name A as a write target.

Each case has a positive control showing the detector is live in this fixture
(an unknown verb really on the protected path; a Python program that really
writes the protected file), so an "allow" here cannot come from a dead binary.

DOES NOT PROVE: which internal stage misparses (heredoc splitting, interpreter
payload extraction, open() argument pairing); nor anything about the subagent
relay path — the payload deliberately carries no agent_id so nothing is queued.

Hermetic: the binary is the one `cargo build -p blastguard` produces from this
checkout; HOME, approvals, block-log and relay dirs point into a temp dir.
"""

import json
import os
import shutil
import subprocess
import tempfile
import unittest
from pathlib import Path

SRC = Path(__file__).resolve().parent.parent
CACHE_LINE = "unknown dirs directly under ~/.claude/plugins/cache/yukineko/ are removed."


def build_blastguard():
    env = dict(os.environ)
    env["PATH"] = os.path.expanduser("~/.cargo/bin") + os.pathsep + env.get("PATH", "")
    p = subprocess.run(
        ["cargo", "build", "-q", "-p", "blastguard", "--bin", "blastguard", "--message-format=json"],
        cwd=SRC, env=env, capture_output=True, text=True, timeout=1800,
    )
    if p.returncode != 0:
        raise RuntimeError("cargo build -p blastguard failed (%d):\n%s" % (p.returncode, p.stderr[-4000:]))
    exe = None
    for line in p.stdout.splitlines():
        try:
            msg = json.loads(line)
        except ValueError:
            continue
        if msg.get("reason") == "compiler-artifact" and msg.get("executable") \
                and msg.get("target", {}).get("name") == "blastguard":
            exe = msg["executable"]
    if not exe or not os.access(exe, os.X_OK):
        raise RuntimeError("cargo build produced no blastguard executable:\n" + p.stdout[-2000:])
    return exe


class HeredocLiteralFalsePositives(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.bin = build_blastguard()
        cls.home = Path(tempfile.mkdtemp(prefix="bg-heredoc-fp-")).resolve()
        (cls.home / ".claude/plugins/cache/yukineko/blastguard/1.0.0").mkdir(parents=True)
        cls.repo = cls.home / "repo"
        cls.repo.mkdir()
        subprocess.run(["git", "init", "-q", str(cls.repo)], check=True, capture_output=True,
                       env={"PATH": os.environ["PATH"], "HOME": str(cls.home),
                            "GIT_CONFIG_NOSYSTEM": "1", "GIT_CONFIG_GLOBAL": os.devnull})
        (cls.repo / ".githooks").mkdir()
        (cls.repo / ".githooks" / "pre-push").write_text("#!/bin/sh\nexit 0\n")
        cls.file_a = cls.home / "data" / "a.jsonl"
        cls.file_a.parent.mkdir()
        cls.file_a.write_text('{"id": "x"}\n')
        cls.file_b = cls.home / "scratch" / "b.txt"  # does not exist
        cls.file_b.parent.mkdir()
        cls.env = {
            "PATH": os.environ["PATH"],
            "HOME": str(cls.home),
            "LC_ALL": "C",
            "CLAUDECODE": "1",
            "CLAUDE_CODE_ENTRYPOINT": "cli",
            "BLASTGUARD_APPROVALS_DIR": str(cls.home / "approvals"),
            "BLASTGUARD_BLOCK_LOG_DIR": str(cls.home / "blocks"),
            "BLASTGUARD_RELAY_DIR": str(cls.home / "relayq"),
        }

    @classmethod
    def tearDownClass(cls):
        shutil.rmtree(cls.home, True)

    def verdict(self, command):
        payload = {
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": {"command": command},
            "cwd": str(self.repo),
        }
        p = subprocess.run([self.bin], input=json.dumps(payload), cwd=self.repo, env=self.env,
                           capture_output=True, text=True, timeout=60)
        self.assertEqual(p.returncode, 0, "hook must exit 0: %r\n%s" % (command, p.stderr))
        out = p.stdout.strip()
        if not out:
            return "allow", ""
        hso = json.loads(out)["hookSpecificOutput"]
        return hso["permissionDecision"], hso.get("permissionDecisionReason", "")

    # --- A: quoted heredoc body + redirect on the operator line -----------------

    def test_a_commit_quoted_heredoc_with_redirect_is_allowed(self):
        cmd = "git commit -q -F - <<'EOF' 2>&1\nsubject\n\n%s\nEOF" % CACHE_LINE
        d, why = self.verdict(cmd)
        self.assertEqual(d, "allow", why)

    def test_a_control_same_heredoc_without_redirect_is_allowed(self):
        cmd = "git commit -q -F - <<'EOF'\nsubject\n\n%s\nEOF" % CACHE_LINE
        d, why = self.verdict(cmd)
        self.assertEqual(d, "allow", why)

    def test_a_control_unknown_verb_really_on_cache_path_is_not_allowed(self):
        d, why = self.verdict("frobnicate ~/.claude/plugins/cache/yukineko/")
        self.assertIn(d, ("ask", "deny"), why)

    # --- B: Python string literal in a python heredoc ----------------------------

    def test_b_python_string_literal_naming_githook_is_allowed(self):
        cmd = "python3 - <<'EOF'\nprint('''Unit tests for .githooks/pre-push''')\nEOF"
        d, why = self.verdict(cmd)
        self.assertEqual(d, "allow", why)

    def test_b_control_python_really_writing_githook_is_not_allowed(self):
        cmd = "python3 - <<'EOF'\nopen('.githooks/pre-push','w').write('x')\nEOF"
        d, why = self.verdict(cmd)
        self.assertIn(d, ("ask", "deny"), why)

    # --- C: read A + write new B is misattributed as a write to A ----------------

    def _read_a_write_b(self):
        return ("python3 - <<'E'\nfor l in open('%s'):\n    open('%s','w').write(l)\nE"
                % (self.file_a, self.file_b))

    def test_c_read_existing_write_new_does_not_name_read_file_as_write_target(self):
        d, why = self.verdict(self._read_a_write_b())
        self.assertNotIn(str(self.file_a), why, "verdict %s names the READ file:\n%s" % (d, why))

    def test_c_read_existing_write_new_gets_same_verdict_as_write_new_only(self):
        d_only_b, why_b = self.verdict("python3 - <<'E'\nopen('%s','w').write('x')\nE" % self.file_b)
        d, why = self.verdict(self._read_a_write_b())
        self.assertEqual(d, d_only_b, "write-B-only: %s (%s)\nread-A+write-B: %s (%s)"
                         % (d_only_b, why_b, d, why))

    def test_c_control_read_only_is_allowed(self):
        d, why = self.verdict("python3 - <<'E'\nfor l in open('%s'):\n    print(l)\nE" % self.file_a)
        self.assertEqual(d, "allow", why)

    def test_c_control_really_overwriting_existing_a_is_not_allowed(self):
        d, why = self.verdict("python3 - <<'E'\nopen('%s','w').write('x')\nE" % self.file_a)
        self.assertIn(d, ("ask", "deny"), why)


if __name__ == "__main__":
    unittest.main()
