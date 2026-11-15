#!/usr/bin/env python3
"""Tests for scripts/blastguard_fp_replay.py, driven through a fake binary.

The fake "blastguard" records the payload and env it was given, then answers
according to the command in the payload, so the tests observe what the replay
actually sends and how it reads every kind of answer.
"""

from __future__ import annotations

import io
import json
import os
import shutil
import stat
import sys
import tempfile
import unittest
from contextlib import redirect_stdout

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
import blastguard_fp_replay as replay  # noqa: E402

FAKE = """#!{python}
import json, os, sys
raw = sys.stdin.read()
p = json.loads(raw)
rec = os.environ["FAKE_BG_RECORD"]
with open(rec, "a") as fh:
    fh.write(json.dumps({{"payload": p, "env": dict(os.environ), "cwd": os.getcwd()}}) + "\\n")
cmd = (p.get("tool_input") or {{}}).get("command", "")
def out(d):
    print(json.dumps({{"hookSpecificOutput": {{"hookEventName": "PreToolUse",
        "permissionDecision": d, "permissionDecisionReason": "why-" + d}}}}))
if cmd.startswith("ALLOW"):
    pass
elif cmd.startswith("DENY"):
    out("deny")
elif cmd.startswith("ASK"):
    out("ask")
elif cmd.startswith("EXIT2"):
    sys.stderr.write("panic\\n"); sys.exit(2)
elif cmd.startswith("GARBAGE"):
    print("not json at all")
elif cmd.startswith("TWOLINES"):
    out("deny"); out("deny")
elif cmd.startswith("BADWORD"):
    out("allow")
"""


def record(command, *, actor="interactive", agent_id=None, cwd=None, capped=False,
           tool_input="auto", sha="auto", decision="deny"):
    ti = {"command": command} if tool_input == "auto" else tool_input
    return {
        "actor": actor, "agent_id": agent_id, "blastguard_version": "0.0.0",
        "command": command, "cwd": cwd, "decision": decision, "decision_kind": decision,
        "file_path": None, "original_decision": decision, "reason": "r", "rule_id": "x",
        "session_id": "sess-1", "system_harm": False,
        "tool_input": None if capped else ti, "tool_input_bytes": 1,
        "tool_input_capped": capped,
        "tool_input_sha256": (replay.tool_input_sha(ti) if sha == "auto" else sha),
        "tool_name": "Bash", "ts": 1700000000, "v": 2,
    }


class Base(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.mkdtemp(prefix="bg-fp-replay-test-")
        self.cwd = os.path.join(self.tmp, "proj")
        os.makedirs(self.cwd)
        self.binary = os.path.join(self.tmp, "fake-blastguard")
        with open(self.binary, "w") as fh:
            fh.write(FAKE.format(python=sys.executable))
        os.chmod(self.binary, os.stat(self.binary).st_mode | stat.S_IXUSR)
        self.rec_path = os.path.join(self.tmp, "calls.jsonl")
        self._saved = {k: os.environ.get(k) for k in
                       ("FAKE_BG_RECORD", "CLAUDE_CODE_SESSION_ID", "BLASTGUARD_ASK")}
        os.environ["FAKE_BG_RECORD"] = self.rec_path
        # Leaks the replay must strip from the judged call's env.
        os.environ["CLAUDE_CODE_SESSION_ID"] = "leaked-session"
        os.environ["BLASTGUARD_ASK"] = "always"

    def tearDown(self):
        for k, v in self._saved.items():
            if v is None:
                os.environ.pop(k, None)
            else:
                os.environ[k] = v
        shutil.rmtree(self.tmp, ignore_errors=True)

    def calls(self):
        if not os.path.exists(self.rec_path):
            return []
        with open(self.rec_path) as fh:
            return [json.loads(x) for x in fh if x.strip()]

    def write_log(self, recs):
        path = os.path.join(self.tmp, "blocks.jsonl")
        with open(path, "w") as fh:
            for r in recs:
                fh.write((r if isinstance(r, str) else json.dumps(r)) + "\n")
        return path

    def replay_one(self, rec, home_mode="sandbox"):
        return replay.replay_record(rec, self.binary, home_mode, self.cwd)


class PayloadRebuild(Base):
    def test_main_session_payload_has_no_agent_id_and_interactive_env(self):
        d, reason, _ = self.replay_one(record("DENY x", cwd=self.cwd))
        self.assertEqual((d, reason), ("deny", "why-deny"))
        [call] = self.calls()
        self.assertEqual(call["payload"], {
            "session_id": "sess-1", "hook_event_name": "PreToolUse", "cwd": self.cwd,
            "tool_name": "Bash", "tool_input": {"command": "DENY x"},
        })
        env = call["env"]
        self.assertEqual(env["CLAUDECODE"], "1")
        self.assertEqual(env["CLAUDE_CODE_ENTRYPOINT"], "cli")
        self.assertNotIn("CLAUDE_CODE_SESSION_ID", env)
        self.assertNotIn("BLASTGUARD_ASK", env)
        self.assertEqual(env["CLAUDE_PROJECT_DIR"], self.cwd)
        self.assertEqual(os.path.realpath(call["cwd"]), os.path.realpath(self.cwd))

    def test_subagent_payload_carries_agent_id(self):
        rec = record("ASK y", actor="subagent", agent_id="agent-42", cwd=self.cwd)
        self.assertEqual(self.replay_one(rec)[0], "ask")
        [call] = self.calls()
        self.assertEqual(call["payload"]["agent_id"], "agent-42")
        self.assertEqual(call["env"]["CLAUDE_CODE_ENTRYPOINT"], "cli")

    def test_headless_actor_gets_non_interactive_entrypoint(self):
        self.replay_one(record("DENY", actor="headless", cwd=self.cwd))
        [call] = self.calls()
        self.assertEqual(call["env"]["CLAUDE_CODE_ENTRYPOINT"], "sdk-cli")
        self.assertNotIn("agent_id", call["payload"])

    def test_state_is_isolated_in_a_temp_dir(self):
        self.replay_one(record("DENY", cwd=self.cwd), home_mode="sandbox")
        self.replay_one(record("DENY", cwd=self.cwd), home_mode="temp")
        sandbox, temp = [c["env"] for c in self.calls()]
        self.assertEqual(sandbox["HOME"], os.environ["HOME"])
        self.assertNotEqual(sandbox["HARNESS_SANDBOX_HOME"], os.environ["HOME"])
        self.assertEqual(temp["HOME"], temp["HARNESS_SANDBOX_HOME"])
        self.assertNotEqual(temp["HOME"], os.environ["HOME"])
        for env in (sandbox, temp):
            for k in ("BLASTGUARD_BLOCK_LOG_DIR", "BLASTGUARD_RELAY_DIR",
                      "BLASTGUARD_APPROVALS_DIR"):
                self.assertTrue(env[k].startswith(env["HARNESS_SANDBOX_HOME"]), k)

    def test_sandbox_precondition_refuses_a_non_account_home(self):
        self.assertIsNone(replay.sandbox_precondition(
            {"HOME": __import__("pwd").getpwuid(os.getuid()).pw_dir}))
        self.assertIsNotNone(replay.sandbox_precondition({"HOME": self.tmp}))
        self.assertIsNotNone(replay.sandbox_precondition({}))

    def test_allow_is_silent_exit_zero(self):
        self.assertEqual(self.replay_one(record("ALLOW", cwd=self.cwd))[:2], ("allow", ""))


class Undetermined(Base):
    def assertUndetermined(self, rec, needle):
        d, reason, _ = self.replay_one(rec)
        self.assertEqual(d, "undetermined", reason)
        self.assertIn(needle, reason)

    def test_capped_input_is_not_replayed(self):
        self.assertUndetermined(record("ALLOW", cwd=self.cwd, capped=True), "tool_input_capped")
        self.assertEqual(self.calls(), [])

    def test_missing_tool_input(self):
        self.assertUndetermined(record("ALLOW", cwd=self.cwd, tool_input=None),
                                "tool_input missing")

    def test_sha_mismatch(self):
        self.assertUndetermined(record("ALLOW", cwd=self.cwd, sha="0" * 64),
                                "tool_input_sha256")

    def test_missing_or_gone_cwd(self):
        self.assertUndetermined(record("ALLOW", cwd=None), "cwd missing")
        self.assertUndetermined(record("ALLOW", cwd=os.path.join(self.tmp, "gone")),
                                "no longer exists")

    def test_unknown_actor_and_subagent_without_agent_id(self):
        self.assertUndetermined(record("ALLOW", cwd=self.cwd, actor="unknown"), "actor")
        self.assertUndetermined(record("ALLOW", cwd=self.cwd, actor="subagent"),
                                "without agent_id")

    def test_bad_exit(self):
        self.assertUndetermined(record("EXIT2", cwd=self.cwd), "binary exit 2")

    def test_bad_output(self):
        self.assertUndetermined(record("GARBAGE", cwd=self.cwd), "unparseable output")
        self.assertUndetermined(record("TWOLINES", cwd=self.cwd), "more than one line")
        self.assertUndetermined(record("BADWORD", cwd=self.cwd), "unexpected decision")

    def test_unparseable_log_line(self):
        log = self.write_log(["{not json", record("DENY", cwd=self.cwd)])
        res = replay.replay_log(log, self.binary, "sandbox", self.cwd)
        self.assertEqual([r["replay_decision"] for r in res], ["undetermined", "deny"])
        self.assertEqual([r["line"] for r in res], [1, 2])


class Summary(Base):
    def test_counts_and_correct_to_allow_listing(self):
        recs = [
            record("ALLOW fp fixed", cwd=self.cwd),        # 1 fp -> allow
            record("DENY fp still", cwd=self.cwd),         # 2 fp -> deny
            record("ASK correct still", cwd=self.cwd),     # 3 correct -> ask
            record("ALLOW correct lost", cwd=self.cwd),    # 4 correct -> allow (regression)
            record("EXIT2", cwd=self.cwd),                 # 5 correct -> undetermined
            record("ALLOW", cwd=self.cwd, capped=True),    # 6 unlabeled -> undetermined
            record("DENY unknown", cwd=self.cwd),          # 7 unknown -> deny
        ]
        log = self.write_log(recs)
        labels_path = os.path.join(self.tmp, "labels.jsonl")
        with open(labels_path, "w") as fh:
            for line, label in [(1, "fp"), (2, "fp"), (3, "correct"), (4, "correct"),
                                (5, "correct"), (7, "unknown")]:
                fh.write(json.dumps({"line": line, "label": label, "why": f"w{line}"}) + "\n")
        out = os.path.join(self.tmp, "out.jsonl")
        buf = io.StringIO()
        with redirect_stdout(buf):
            rc = replay.main(["--binary", self.binary, "--log", log, "--out", out,
                              "--labels", labels_path, "--summary", "--home-mode", "temp",
                              "--project-dir", self.cwd])
        self.assertEqual(rc, 0)
        with open(out) as fh:
            rows = [json.loads(x) for x in fh]
        self.assertEqual(len(rows), 7)
        self.assertEqual(set(rows[0]), {"line", "ts", "tool_name", "tool_input_sha256", "actor",
                                        "logged_decision", "logged_system_harm",
                                        "replay_decision", "replay_reason", "replay_notes"})
        s = replay.summarize(rows, replay.load_labels(labels_path))
        self.assertEqual(s["overall"], {"allow": 2, "ask": 1, "deny": 2, "undetermined": 2})
        self.assertEqual(s["fp_still_blocked"], 1)
        self.assertEqual(s["fp_became_allow"], 1)
        self.assertEqual(s["correct_still_blocked"], 1)
        self.assertEqual([c["line"] for c in s["correct_became_allow"]], [4])
        self.assertEqual(s["undetermined"], 2)
        self.assertEqual(s["per_label"]["correct"],
                         {"allow": 1, "ask": 1, "deny": 0, "undetermined": 1})
        self.assertEqual(s["per_label"]["unlabeled"]["undetermined"], 1)
        text = buf.getvalue()
        self.assertIn("CORRECT->ALLOW line=4", text)
        self.assertIn("correct became allow: 1", text)
        self.assertIn("undetermined: 2", text)

    def test_bad_label_is_rejected(self):
        p = os.path.join(self.tmp, "labels.jsonl")
        with open(p, "w") as fh:
            fh.write(json.dumps({"line": 1, "label": "maybe"}) + "\n")
        with self.assertRaises(ValueError):
            replay.load_labels(p)


if __name__ == "__main__":
    unittest.main()
