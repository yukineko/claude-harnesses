#!/usr/bin/env python3
"""Replay blastguard's block log through a blastguard binary (FP measuring stick).

Reads blastguard's block log (``~/.blastguard/blocks/blocks.jsonl``, one record
per Deny/Ask blastguard printed; see ``crates/blastguard/src/blocklog.rs``),
rebuilds the PreToolUse hook payload for each record, runs ``--binary`` on it
the way the plugin hook does (``hooks/hooks.json``: plain ``blastguard`` with
the payload on stdin, no arguments; ``bin/blastguard`` only dispatches to the
per-platform build), and writes one JSONL result per log line:

    {line, ts, tool_name, tool_input_sha256, actor, logged_decision,
     logged_system_harm, replay_decision, replay_reason, replay_notes}

``replay_decision`` is ``allow`` | ``ask`` | ``deny`` | ``undetermined``.

How a decision is read (``crates/blastguard/src/hookio.rs``): ``allow`` is
exit 0 with NOTHING on stdout; ``deny``/``ask`` is exit 0 with one JSON line
whose ``hookSpecificOutput.permissionDecision`` is that word. Anything else --
a non-zero exit (exit 2 is blastguard's panic block), a timeout, stdout that is
not exactly that shape -- is ``undetermined``. A line that cannot be replayed
faithfully is ``undetermined`` with the cause in ``replay_reason`` and is never
reported as ``allow``:

  * the log line is not a JSON object;
  * ``tool_input`` is missing/null, or ``tool_input_capped`` is true (the log
    dropped the copy, so the call cannot be rebuilt);
  * the sha256 of the logged ``tool_input`` re-serialized the way blastguard
    serializes it (serde_json compact, keys sorted, non-ASCII verbatim) does
    not match ``tool_input_sha256`` (the copy is not provably the input);
  * ``cwd`` is missing or no longer exists on disk;
  * ``actor`` is not one of ``interactive`` / ``headless`` / ``subagent``, or
    is ``subagent`` without an ``agent_id`` (the mode cannot be rebuilt);
  * the binary exits non-zero, times out, or prints unparseable output.

Payload rebuilt (what ``main.rs::run`` reads): ``session_id``,
``hook_event_name: "PreToolUse"``, ``cwd``, ``tool_name``, ``tool_input``, and
``agent_id`` only when the record has one (its presence is what
``interactive::payload_is_subagent`` uses to detect a subagent).

Mode rebuilt from ``actor`` (``interactive::ask_available``):
  * ``interactive`` -> ``CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=cli``
  * ``headless``    -> ``CLAUDECODE=1 CLAUDE_CODE_ENTRYPOINT=sdk-cli``
  * ``subagent``    -> ``agent_id`` in the payload, env as ``interactive``
    (a subagent's Ask is a Deny either way; only the relay annotation in the
    reason depends on the parent's env, and that annotation is not compared).
``BLASTGUARD_ASK`` and ``CLAUDE_CODE_SESSION_ID`` are removed.

ISOLATION (the replay never writes the real blastguard/overwatch state).
Every state write blastguard makes on the verdict path -- block log, relay
queue, approval memory, repeat-deny ledger, overwatch violation ledger -- goes
under ``harness_core::config::home()``. Each run gets a fresh temp dir, and:

  * ``BLASTGUARD_BLOCK_LOG_DIR``, ``BLASTGUARD_RELAY_DIR`` and
    ``BLASTGUARD_APPROVALS_DIR`` point into it;
  * ``--home-mode sandbox`` (default): ``HOME`` stays the real home (several
    detectors read ``$HOME`` directly to judge paths -- ``scope::SafeRoots``,
    ``detect::protected``, ``detect::scratch_place`` -- so a fake ``HOME``
    would change the verdict being measured) and ``HARNESS_SANDBOX_HOME`` is
    the temp dir. ``harness_core::config::resolve_home`` returns the sandbox
    exactly when ``HOME`` equals the passwd account home; this script checks
    that precondition up front and refuses to run in this mode otherwise.
  * ``--home-mode temp``: ``HOME`` and ``HARNESS_SANDBOX_HOME`` are both the
    temp dir. Isolation does not depend on any precondition, but every
    ``$HOME``-relative judgment is made against the temp dir, not the home the
    original call saw.

WHAT STILL DIFFERS FROM THE ORIGINAL CALL (the replay is an APPROXIMATION):
  * the filesystem: files, worktrees and git state the original call saw may
    be gone or changed (a still-existing cwd is not the same tree);
  * ``CLAUDE_PROJECT_DIR`` is not logged; it is approximated as the main
    worktree root of ``cwd`` (``git rev-parse --git-common-dir``), falling back
    to ``cwd`` (noted in ``replay_notes``), or forced with ``--project-dir``;
  * ``TMPDIR`` is inherited from this process, not the original hook's env;
  * approval memory and the repeat-deny ledger start empty, so a logged
    decision reached through "approved before" or "2nd identical request ->
    ask" is not reproduced (``original_decision`` vs ``decision`` in the log
    shows the latter);
  * the parent session's interactivity for a ``subagent`` record;
  * the binary judging now is ``--binary``, not ``blastguard_version``.

Labels and summary: ``--labels FILE`` is JSONL ``{line, label, why}`` with
``label`` in ``fp`` | ``correct`` | ``unknown``. ``--summary`` prints the
replay decision counts overall and per label, FP still Deny/Ask, FP that
became Allow, correct still Deny/Ask, every correct record that became Allow
individually, and the undetermined count with its causes.

stdlib only.
"""

from __future__ import annotations

import argparse
import collections
import hashlib
import json
import os
import pwd
import subprocess
import sys
import tempfile
from typing import Any

UNDETERMINED = "undetermined"
DECISIONS = ("allow", "ask", "deny", UNDETERMINED)
LABELS = ("fp", "correct", "unknown")
HOOK_TIMEOUT_S = 10  # hooks/hooks.json: "timeout": 10

ACTOR_ENV = {
    "interactive": {"CLAUDECODE": "1", "CLAUDE_CODE_ENTRYPOINT": "cli"},
    "headless": {"CLAUDECODE": "1", "CLAUDE_CODE_ENTRYPOINT": "sdk-cli"},
    "subagent": {"CLAUDECODE": "1", "CLAUDE_CODE_ENTRYPOINT": "cli"},
}

# Removed from the inherited env so the replaying process's own Claude Code /
# blastguard settings never leak into the judged call.
STRIP_PREFIXES = ("CLAUDE", "BLASTGUARD_", "HARNESS_")


def serde_compact(value: Any) -> str:
    """``serde_json::to_string`` of a ``Value`` without ``preserve_order``."""
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False, sort_keys=True)


def tool_input_sha(value: Any) -> str:
    return hashlib.sha256(serde_compact(value).encode("utf-8")).hexdigest()


def build_payload(rec: dict) -> dict:
    """The PreToolUse payload blastguard's hook reads, rebuilt from a record."""
    payload = {
        "session_id": rec.get("session_id") or "",
        "hook_event_name": "PreToolUse",
        "cwd": rec.get("cwd") or "",
        "tool_name": rec.get("tool_name") or "",
        "tool_input": rec.get("tool_input"),
    }
    if rec.get("agent_id") is not None:
        payload["agent_id"] = rec["agent_id"]
    return payload


def build_env(actor: str, state_dir: str, home_mode: str, project_dir: str,
              base_env: dict | None = None) -> dict:
    src = dict(os.environ if base_env is None else base_env)
    env = {k: v for k, v in src.items() if not k.startswith(STRIP_PREFIXES)}
    env.update(ACTOR_ENV[actor])
    env["HARNESS_SANDBOX_HOME"] = state_dir
    if home_mode == "temp":
        env["HOME"] = state_dir
    env["BLASTGUARD_BLOCK_LOG_DIR"] = os.path.join(state_dir, "bg-blocks")
    env["BLASTGUARD_RELAY_DIR"] = os.path.join(state_dir, "bg-relay")
    env["BLASTGUARD_APPROVALS_DIR"] = os.path.join(state_dir, "bg-approvals")
    env["CLAUDE_PROJECT_DIR"] = project_dir
    return env


def sandbox_precondition(environ: dict | None = None) -> str | None:
    """None when ``HOME`` is the passwd home (sandbox mode isolates), else why not."""
    env = os.environ if environ is None else environ
    home = env.get("HOME", "")
    if not home.strip():
        return "HOME is unset/empty"
    try:
        acct = pwd.getpwuid(os.getuid()).pw_dir
    except KeyError as e:
        return f"account home undeterminable: {e}"
    if os.path.realpath(home) != os.path.realpath(acct):
        return f"HOME={home!r} is not the account home {acct!r}"
    return None


def project_dir_for(cwd: str) -> tuple[str, str | None]:
    """(project dir, note or None)."""
    try:
        out = subprocess.run(
            ["git", "-C", cwd, "rev-parse", "--path-format=absolute", "--git-common-dir"],
            capture_output=True, text=True, timeout=10,
        )
    except (OSError, subprocess.TimeoutExpired) as e:
        return cwd, f"project_dir=cwd (git failed: {e})"
    if out.returncode != 0 or not out.stdout.strip():
        return cwd, f"project_dir=cwd (git rev-parse exit {out.returncode})"
    common = out.stdout.strip()
    if os.path.basename(common) == ".git":
        return os.path.dirname(common), None
    return cwd, f"project_dir=cwd (git common dir {common!r} is not a .git dir)"


def parse_output(code: int | None, stdout: str) -> tuple[str, str]:
    """(decision, reason) from the binary's exit code and stdout."""
    if code != 0:
        return UNDETERMINED, f"binary exit {code}"
    text = stdout.strip()
    if text == "":
        return "allow", ""
    if "\n" in text:
        return UNDETERMINED, "unparseable output: more than one line"
    try:
        doc = json.loads(text)
        hso = doc["hookSpecificOutput"]
        decision = hso["permissionDecision"]
        reason = hso.get("permissionDecisionReason", "")
    except (ValueError, KeyError, TypeError) as e:
        return UNDETERMINED, f"unparseable output: {e!r}: {text[:200]!r}"
    if hso.get("hookEventName") != "PreToolUse" or decision not in ("deny", "ask"):
        return UNDETERMINED, f"unparseable output: unexpected decision {decision!r}"
    if not isinstance(reason, str):
        return UNDETERMINED, "unparseable output: reason is not a string"
    return decision, reason


def precheck(rec: Any) -> str | None:
    """The cause a record cannot be replayed faithfully, or None."""
    if not isinstance(rec, dict):
        return "log line is not a JSON object"
    if rec.get("tool_input_capped") is True:
        return "tool_input_capped: the log dropped tool_input"
    if rec.get("tool_input") is None:
        return "tool_input missing in the log line"
    want = rec.get("tool_input_sha256")
    if not isinstance(want, str) or tool_input_sha(rec["tool_input"]) != want:
        return "tool_input_sha256 does not match the logged tool_input"
    actor = rec.get("actor")
    if actor not in ACTOR_ENV:
        return f"actor {actor!r} cannot be rebuilt"
    if actor == "subagent" and rec.get("agent_id") is None:
        return "actor subagent without agent_id"
    cwd = rec.get("cwd")
    if not isinstance(cwd, str) or not cwd:
        return "cwd missing in the log line"
    if not os.path.isdir(cwd):
        return "cwd no longer exists on disk"
    return None


def replay_record(rec: dict, binary: str, home_mode: str,
                  forced_project_dir: str | None) -> tuple[str, str, list]:
    notes: list = []
    cause = precheck(rec)
    if cause is not None:
        return UNDETERMINED, cause, notes
    if forced_project_dir is not None:
        project_dir = forced_project_dir
    else:
        project_dir, note = project_dir_for(rec["cwd"])
        if note:
            notes.append(note)
    payload = json.dumps(build_payload(rec), ensure_ascii=False)
    with tempfile.TemporaryDirectory(prefix="bg-fp-replay-") as state:
        env = build_env(rec["actor"], state, home_mode, project_dir)
        try:
            out = subprocess.run(
                [binary], input=payload.encode("utf-8"), capture_output=True,
                env=env, cwd=rec["cwd"], timeout=HOOK_TIMEOUT_S,
            )
        except subprocess.TimeoutExpired:
            return UNDETERMINED, f"binary timed out after {HOOK_TIMEOUT_S}s", notes
        except OSError as e:
            return UNDETERMINED, f"binary could not be run: {e}", notes
    stdout = out.stdout.decode("utf-8", errors="replace")
    decision, reason = parse_output(out.returncode, stdout)
    if decision == UNDETERMINED and out.stderr:
        reason += f"; stderr: {out.stderr.decode('utf-8', errors='replace')[:300]}"
    return decision, reason, notes


def replay_log(log_path: str, binary: str, home_mode: str = "sandbox",
               forced_project_dir: str | None = None) -> list:
    results = []
    with open(log_path, encoding="utf-8", errors="replace") as fh:
        for lineno, raw in enumerate(fh, start=1):
            if not raw.strip():
                continue
            try:
                rec = json.loads(raw)
            except ValueError:
                rec = None
            if isinstance(rec, dict):
                decision, reason, notes = replay_record(rec, binary, home_mode, forced_project_dir)
            else:
                decision, reason, notes = UNDETERMINED, "log line is not a JSON object", []
                rec = {}
            results.append({
                "line": lineno,
                "ts": rec.get("ts"),
                "tool_name": rec.get("tool_name"),
                "tool_input_sha256": rec.get("tool_input_sha256"),
                "actor": rec.get("actor"),
                "logged_decision": rec.get("decision"),
                "logged_system_harm": rec.get("system_harm"),
                "replay_decision": decision,
                "replay_reason": reason,
                "replay_notes": notes,
            })
    return results


def load_labels(path: str) -> dict:
    labels = {}
    with open(path, encoding="utf-8") as fh:
        for n, raw in enumerate(fh, start=1):
            if not raw.strip():
                continue
            row = json.loads(raw)
            if not isinstance(row, dict) or not isinstance(row.get("line"), int):
                raise ValueError(f"labels line {n}: needs an integer 'line'")
            if row.get("label") not in LABELS:
                raise ValueError(f"labels line {n}: label must be one of {LABELS}")
            if row["line"] in labels:
                raise ValueError(f"labels line {n}: duplicate label for log line {row['line']}")
            labels[row["line"]] = row
    return labels


def summarize(results: list, labels: dict) -> dict:
    overall = collections.Counter(r["replay_decision"] for r in results)
    per_label: dict = {lab: collections.Counter() for lab in LABELS + ("unlabeled",)}
    correct_to_allow = []
    causes = collections.Counter()
    for r in results:
        lab = labels.get(r["line"], {}).get("label", "unlabeled")
        per_label[lab][r["replay_decision"]] += 1
        if r["replay_decision"] == UNDETERMINED:
            causes[r["replay_reason"].split(";")[0]] += 1
        if lab == "correct" and r["replay_decision"] == "allow":
            correct_to_allow.append({
                "line": r["line"], "ts": r["ts"], "tool_name": r["tool_name"],
                "why": labels[r["line"]].get("why"),
            })
    blocked = ("deny", "ask")
    return {
        "total": len(results),
        "overall": {d: overall.get(d, 0) for d in DECISIONS},
        "per_label": {lab: {d: c.get(d, 0) for d in DECISIONS} for lab, c in per_label.items()},
        "fp_still_blocked": sum(per_label["fp"][d] for d in blocked),
        "fp_became_allow": per_label["fp"]["allow"],
        "correct_still_blocked": sum(per_label["correct"][d] for d in blocked),
        "correct_became_allow": correct_to_allow,
        "undetermined": overall.get(UNDETERMINED, 0),
        "undetermined_causes": dict(causes.most_common()),
    }


def render_summary(s: dict) -> str:
    lines = [f"records: {s['total']}"]
    lines.append("overall: " + " ".join(f"{d}={n}" for d, n in s["overall"].items()))
    for lab, counts in s["per_label"].items():
        lines.append(f"label {lab}: " + " ".join(f"{d}={n}" for d, n in counts.items()))
    lines.append(f"fp still deny/ask: {s['fp_still_blocked']}")
    lines.append(f"fp became allow: {s['fp_became_allow']}")
    lines.append(f"correct still deny/ask: {s['correct_still_blocked']}")
    lines.append(f"correct became allow: {len(s['correct_became_allow'])}")
    for c in s["correct_became_allow"]:
        lines.append(f"  CORRECT->ALLOW line={c['line']} ts={c['ts']} tool={c['tool_name']} why={c['why']!r}")
    lines.append(f"undetermined: {s['undetermined']}")
    for cause, n in s["undetermined_causes"].items():
        lines.append(f"  {n:5d}  {cause}")
    return "\n".join(lines)


def main(argv: list | None = None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--binary", required=True, help="blastguard binary to judge with")
    ap.add_argument("--log", default=os.path.expanduser("~/.blastguard/blocks/blocks.jsonl"))
    ap.add_argument("--out", required=True, help="JSONL output path")
    ap.add_argument("--labels", help="JSONL {line, label: fp|correct|unknown, why}")
    ap.add_argument("--summary", action="store_true")
    ap.add_argument("--home-mode", choices=("sandbox", "temp"), default="sandbox")
    ap.add_argument("--project-dir", help="force CLAUDE_PROJECT_DIR for every record")
    a = ap.parse_args(argv)

    if not os.access(a.binary, os.X_OK):
        print(f"blastguard_fp_replay: --binary {a.binary!r} is not executable", file=sys.stderr)
        return 2
    if a.home_mode == "sandbox":
        why = sandbox_precondition()
        if why is not None:
            print(f"blastguard_fp_replay: --home-mode sandbox would not isolate state: {why}; "
                  "use --home-mode temp", file=sys.stderr)
            return 2
    labels = load_labels(a.labels) if a.labels else {}
    binary = os.path.abspath(a.binary)
    results = replay_log(a.log, binary, a.home_mode, a.project_dir)
    with open(a.out, "w", encoding="utf-8") as fh:
        for r in results:
            fh.write(json.dumps(r, ensure_ascii=False) + "\n")
    if a.summary:
        print(render_summary(summarize(results, labels)))
    return 0


if __name__ == "__main__":
    sys.exit(main())
