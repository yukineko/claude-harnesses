#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (user ruling 2026-11-16, 「表示をやめてほしい」): `blastguard relay stop`
//! no longer lists dismissed relay entries (neither the summary line nor the
//! per-id / reason lines). Pending entries are still listed and still block;
//! the dismissal stays recorded in the queue file; `relay dismiss` still
//! prints its confirmation.
//!
//! DOES NOT PROVE: the fail-closed paths (unreadable / corrupt queue ->
//! Block), which are covered by relay_fail_closed_integration_20261114.rs and
//! subagent_ask_relay_20261114.rs; nor what the real Claude Code does with the
//! Stop output.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

const SESSION: &str = "relay-hide-sess-20261116";
const AGENT: &str = "a0c1d2e3f4a5b6c7d";

fn setup(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!("bg-relay-hide-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    for d in ["home", "approvals", "relay", "cwd", "other"] {
        std::fs::create_dir_all(root.join(d)).unwrap();
    }
    // The session transcript names the subagent, so the entry is not a forged
    // one (see backlog_20261116_relay_forged_session_entry.rs).
    std::fs::write(
        root.join("transcript.jsonl"),
        format!(
            "{}\n{}\n",
            json!({"type":"user","sessionId":SESSION,"message":{"role":"user","content":"hi"}}),
            json!({"type":"assistant","sessionId":SESSION,"agentId":AGENT,"isSidechain":true,
                   "message":{"role":"assistant","content":"working"}})
        ),
    )
    .unwrap();
    root
}

fn run(root: &Path, args: &[&str], stdin: &str) -> Output {
    let mut c = bounded::command_for(args);
    c.env("HOME", root.join("home"))
        // TMPDIR is a safe root; keep it off `other/` so `other/` is not this
        // session's tree (as the sibling worktree was in the observation).
        .env("TMPDIR", root.join("home"))
        .env("BLASTGUARD_RELAY_DIR", root.join("relay"))
        .env("BLASTGUARD_APPROVALS_DIR", root.join("approvals"))
        .env("CLAUDE_CODE_SESSION_ID", "sessEnvIgnored")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_PROJECT_DIR")
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE)
}

fn pre(root: &Path, command: &str, agent: Option<&str>) -> Value {
    let mut p = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
        "cwd": root.join("cwd").to_string_lossy(),
        "session_id": SESSION,
        "transcript_path": root.join("transcript.jsonl").to_string_lossy(),
    });
    if let Some(a) = agent {
        p["agent_id"] = json!(a);
        p["agent_type"] = json!("general-purpose");
    }
    let o = run(root, &[], &p.to_string());
    serde_json::from_slice(&o.stdout).unwrap_or(Value::Null)
}

fn stop_text(root: &Path) -> String {
    let p = json!({
        "hook_event_name": "Stop",
        "session_id": SESSION,
        "cwd": root.join("cwd").to_string_lossy(),
        "transcript_path": root.join("transcript.jsonl").to_string_lossy(),
        "stop_hook_active": false,
    });
    let o = run(root, &["relay", "stop"], &p.to_string());
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn queue_text(root: &Path) -> String {
    std::fs::read_to_string(root.join("relay").join(format!("{SESSION}.jsonl"))).unwrap()
}

fn queue_ask(root: &Path, marker: &str) -> String {
    let before = queue_text_or_empty(root);
    let cmd = format!(
        r#"for f in a b; do git checkout -- "$f"; done; echo {marker} > {}"#,
        root.join("other").join("f.txt").display()
    );
    let v = pre(root, &cmd, Some(AGENT));
    assert_eq!(
        v["hookSpecificOutput"]["permissionDecision"], "deny",
        "control: subagent Ask hardened to deny: {v}"
    );
    let after = queue_text(root);
    let new: Vec<String> = after[before.len()..]
        .lines()
        .filter_map(|l| serde_json::from_str::<Value>(l).ok())
        .filter(|v| v["event"] == "queued")
        .map(|v| v["id"].as_str().unwrap().to_string())
        .collect();
    assert_eq!(
        new.len(),
        1,
        "control: exactly one new queued entry: {after}"
    );
    new[0].clone()
}

fn queue_text_or_empty(root: &Path) -> String {
    std::fs::read_to_string(root.join("relay").join(format!("{SESSION}.jsonl"))).unwrap_or_default()
}

fn dismiss(root: &Path, id: &str, reason: &str) -> Output {
    let o = run(root, &["relay", "dismiss", id, "--reason", reason], "");
    assert_eq!(o.status.code(), Some(0), "control: dismiss: {o:?}");
    o
}

#[test]
fn all_dismissed_stop_allows_with_no_dismissed_text() {
    let root = setup("all");
    let id = queue_ask(&root, "one");
    let pending = stop_text(&root);
    assert!(
        pending.contains(&id) && pending.contains("block"),
        "control: before dismissal the entry is listed and Stop blocks: {pending}"
    );
    let o = dismiss(&root, &id, "REASON-ALL-DISMISSED human declined");
    // `relay dismiss` keeps printing its own confirmation.
    let confirm = format!(
        "{}{}",
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        confirm.contains(&id),
        "dismiss confirmation lost: {confirm:?}"
    );
    let out = stop_text(&root);
    assert!(
        !out.contains("\"decision\":\"block\"") && !out.contains("\"decision\": \"block\""),
        "all entries resolved: Stop must allow: {out}"
    );
    assert!(
        !out.contains("dismissed") && !out.contains(&id) && !out.contains("REASON-ALL-DISMISSED"),
        "Stop output lists a dismissed entry: {out}"
    );
}

#[test]
fn one_dismissed_one_pending_blocks_on_pending_only() {
    let root = setup("mixed");
    let dismissed = queue_ask(&root, "d");
    let pending = queue_ask(&root, "p");
    assert_ne!(dismissed, pending);
    dismiss(&root, &dismissed, "REASON-MIXED-DISMISSED human declined");
    let out = stop_text(&root);
    assert!(
        out.contains("\"block\"") && out.contains(&pending),
        "Stop must block and show the pending id {pending}: {out}"
    );
    assert!(
        !out.contains(&dismissed)
            && !out.contains("REASON-MIXED-DISMISSED")
            && !out.contains("dismissed"),
        "Stop output lists the dismissed entry {dismissed}: {out}"
    );
}

#[test]
fn dismissal_stays_recorded_in_the_queue_file() {
    let root = setup("recorded");
    let id = queue_ask(&root, "r");
    dismiss(&root, &id, "REASON-RECORDED human declined");
    let _ = stop_text(&root);
    let events: Vec<Value> = queue_text(&root)
        .lines()
        .filter_map(|l| serde_json::from_str(l).ok())
        .filter(|v: &Value| v["event"] == "dismissed")
        .collect();
    assert_eq!(events.len(), 1, "{events:?}");
    assert_eq!(events[0]["id"], id.as_str());
    assert_eq!(events[0]["reason"], "REASON-RECORDED human declined");
}
