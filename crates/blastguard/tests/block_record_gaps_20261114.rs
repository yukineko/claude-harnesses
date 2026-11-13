#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Gap tests for the blastguard block log (`blocks.jsonl`) that
//! `block_record_20261114.rs` does not cover. Data fields only; no prose.
//!
//! (a) an Ask hardened to Deny in a HEADLESS main session is recorded as
//!     `ask-hardened-to-deny` with the byte-exact command, cwd and agent_id.
//! (b) a hardened Ask that is then downgraded on repeat (Deny -> Ask) is
//!     recorded; the record must keep the fact that the ORIGINAL verdict was an
//!     Ask (field `original_decision == "ask"`). The current schema has no such
//!     field, so `decision_kind = deny-downgraded-to-ask` is the only trace and
//!     it is indistinguishable from a repeat of a real Deny.
//! (c) a tool_input over 1 MiB sets `tool_input_capped`, carries
//!     `tool_input_bytes` and `tool_input_sha256`, and the sha256 matches the
//!     full serialized input.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::PathBuf;
use std::process::{Command, Stdio};

const ASK_CMD: &str = r#"rm "$f""#;

struct Env {
    home: PathBuf,
    log_dir: PathBuf,
    relay: PathBuf,
    approvals: PathBuf,
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("bg-blockgap-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let e = Env {
        home: root.join("home"),
        log_dir: root.join("blocklog"),
        relay: root.join("relay"),
        approvals: root.join("approvals"),
    };
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::create_dir_all(&e.approvals).unwrap();
    e
}

struct Run {
    stdout: String,
}

/// Always headless (no CLAUDECODE / entrypoint). `session` pins the repeat
/// ledger; `None` makes the run unattributable (never downgraded).
fn run_headless(e: &Env, session: Option<&str>, stdin: &str) -> Run {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.env("HOME", &e.home)
        .env("BLASTGUARD_BLOCK_LOG_DIR", &e.log_dir)
        .env("BLASTGUARD_RELAY_DIR", &e.relay)
        .env("BLASTGUARD_APPROVALS_DIR", &e.approvals)
        .env_remove("BLASTGUARD_ASK")
        .env_remove("CLAUDECODE")
        .env_remove("CLAUDE_CODE_ENTRYPOINT")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    match session {
        Some(s) => {
            c.env("CLAUDE_CODE_SESSION_ID", s);
        }
        None => {
            c.env_remove("CLAUDE_CODE_SESSION_ID");
        }
    }
    let out = bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE);
    Run {
        stdout: String::from_utf8_lossy(&out.stdout).into_owned(),
    }
}

fn printed(r: &Run) -> String {
    if r.stdout.trim().is_empty() {
        return "allow".into();
    }
    let d: Value = serde_json::from_str(r.stdout.trim()).unwrap();
    d["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap()
        .to_string()
}

fn records(e: &Env) -> Vec<Value> {
    match std::fs::read_to_string(e.log_dir.join("blocks.jsonl")) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap())
            .collect(),
        Err(_) => vec![],
    }
}

fn bash(session: &str, cwd: &str, command: &str) -> Value {
    json!({
        "hook_event_name": "PreToolUse",
        "session_id": session,
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
}

#[test]
fn a_headless_hardened_ask_is_recorded_as_ask_hardened_to_deny() {
    let e = env("a");
    let cwd = "/tmp/headless dir/ünï";
    let cmd = "rm \"$f\"";
    let payload = bash("sess-ga", cwd, cmd);
    let r = run_headless(&e, None, &payload.to_string());
    assert_eq!(printed(&r), "deny", "fixture: headless Ask hardens to deny");
    let recs = records(&e);
    assert_eq!(recs.len(), 1, "{recs:?}");
    let rec = &recs[0];
    assert_eq!(rec["decision"], "deny", "{rec}");
    assert_eq!(rec["decision_kind"], "ask-hardened-to-deny", "{rec}");
    assert_eq!(rec["actor"], "headless", "{rec}");
    assert_eq!(rec["command"], cmd, "{rec}");
    assert_eq!(rec["cwd"], cwd, "{rec}");
    assert!(rec["agent_id"].is_null(), "{rec}");
    assert_eq!(rec["session_id"], "sess-ga", "{rec}");
}

#[test]
#[ignore = "RED: deny-downgraded-to-ask record does not keep that the original verdict was an Ask (no original_decision field); a hardened Ask repeated is indistinguishable from a repeated Deny"]
fn b_deny_downgraded_to_ask_keeps_original_ask() {
    let e = env("b");
    let sess = "sess-gb";
    let payload = bash(sess, "/tmp/cwd-gb", ASK_CMD).to_string();

    let first = run_headless(&e, Some(sess), &payload);
    assert_eq!(
        printed(&first),
        "deny",
        "first: headless Ask hardens to deny"
    );
    let second = run_headless(&e, Some(sess), &payload);
    assert_eq!(
        printed(&second),
        "ask",
        "second identical request must be downgraded to ask: {}",
        second.stdout
    );

    let recs = records(&e);
    assert_eq!(recs.len(), 2, "{recs:?}");
    assert_eq!(
        recs[0]["decision_kind"], "ask-hardened-to-deny",
        "{}",
        recs[0]
    );
    let rec = &recs[1];
    assert_eq!(rec["decision"], "ask", "{rec}");
    assert_eq!(rec["decision_kind"], "deny-downgraded-to-ask", "{rec}");
    assert_eq!(rec["command"], ASK_CMD, "{rec}");
    assert_eq!(
        rec["original_decision"], "ask",
        "the record must keep that the detector's original verdict was Ask: {rec}"
    );
}

#[test]
fn c_oversized_tool_input_is_capped_with_length_and_sha256_of_full_input() {
    let e = env("c");
    let content = "x".repeat((1 << 20) + 17);
    let tool_input = json!({ "file_path": "/etc/hosts", "content": content });
    let payload = json!({
        "hook_event_name": "PreToolUse",
        "session_id": "sess-gc",
        "cwd": "/tmp/cwd-gc",
        "tool_name": "Write",
        "tool_input": tool_input,
    });
    let r = run_headless(&e, None, &payload.to_string());
    assert_eq!(printed(&r), "deny", "fixture: Write /etc/hosts must deny");
    let recs = records(&e);
    assert_eq!(recs.len(), 1);
    let rec = &recs[0];
    let ser = serde_json::to_string(&tool_input).unwrap();
    let want: String = Sha256::digest(ser.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    assert_eq!(rec["tool_input_capped"], true);
    assert!(rec["tool_input"].is_null());
    assert_eq!(rec["tool_input_bytes"], ser.len());
    assert_eq!(rec["tool_input_sha256"], want.as_str());
    assert_eq!(rec["file_path"], "/etc/hosts");
}
