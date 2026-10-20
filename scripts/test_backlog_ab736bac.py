#!/usr/bin/env python3
"""Acceptance repro for backlog ab736bac: harness-recur M1 is not implemented.

docs/harness-recur-spec.md section 4 specifies M1: at SessionEnd, extract the
section 6.3 features from the transcript and persist them to `session_feature`
(SQLite under ${CLAUDE_PLUGIN_DATA}/harness-recur/), because transcripts disappear
within ~2 days so storing only `transcript_path` is a dangling pointer.

This test drives the specified interface: build package `harness-recur`, feed
`harness-recur link --event end` a SessionEnd hook payload whose transcript has a
Read of a known file, DELETE the transcript, then require that a
`session_feature` row for that session and target exists in a SQLite file under
$CLAUDE_PLUGIN_DATA/harness-recur/. At f764bfeb there is no crates/harness-recur
(`cargo build -p harness-recur` -> package not found). Open; expectedFailure.
"""

import glob
import json
import os
import sqlite3
import subprocess
import tempfile
import unittest
from pathlib import Path

REPO = Path(__file__).resolve().parent.parent


def _cargo():
    c = Path.home() / ".cargo" / "bin" / "cargo"
    return str(c) if c.exists() else "cargo"


def build_bin():
    p = subprocess.run(
        [_cargo(), "build", "-p", "harness-recur", "--message-format=json"],
        cwd=REPO, capture_output=True, text=True, timeout=1800,
    )
    if p.returncode != 0:
        raise AssertionError("cannot build harness-recur:\n" + p.stderr[-1500:])
    for line in p.stdout.splitlines():
        try:
            d = json.loads(line)
        except ValueError:
            continue
        if d.get("reason") == "compiler-artifact" and d.get("executable") and d["target"]["name"] == "harness-recur":
            return d["executable"]
    raise AssertionError("no harness-recur executable produced")


class M1PersistsFeaturesAtSessionEnd(unittest.TestCase):
    @unittest.expectedFailure
    def test_session_feature_survives_transcript_deletion(self):
        exe = build_bin()
        with tempfile.TemporaryDirectory(prefix="bl-ab736bac-") as d:
            d = Path(d)
            data = d / "plugin-data"
            target = str(d / "src" / "lib.rs")
            transcript = d / "t.jsonl"
            transcript.write_text(json.dumps({
                "type": "assistant",
                "timestamp": "2026-10-02T00:00:00Z",
                "message": {"content": [{
                    "type": "tool_use", "id": "tu1", "name": "Read",
                    "input": {"file_path": target},
                }]},
            }) + "\n")
            payload = {
                "session_id": "sess-ab736bac",
                "transcript_path": str(transcript),
                "cwd": str(d),
                "hook_event_name": "SessionEnd",
                "reason": "other",
            }
            env = dict(os.environ, CLAUDE_PLUGIN_DATA=str(data), HOME=str(d))
            p = subprocess.run([exe, "link", "--event", "end"], input=json.dumps(payload),
                               env=env, cwd=d, capture_output=True, text=True, timeout=60)
            self.assertEqual(p.returncode, 0, p.stderr)
            transcript.unlink()
            dbs = [f for f in glob.glob(str(data / "harness-recur" / "**" / "*"), recursive=True)
                   if os.path.isfile(f)]
            rows = []
            for f in dbs:
                try:
                    with sqlite3.connect(f) as c:
                        rows += c.execute(
                            "SELECT tool_name, target FROM session_feature WHERE session_id = ?",
                            ("sess-ab736bac",),
                        ).fetchall()
                except sqlite3.DatabaseError:
                    continue
            self.assertIn(("Read", target), rows, "session_feature rows: %r (files: %r)" % (rows, dbs))


if __name__ == "__main__":
    unittest.main()
