// このファイルは丸ごと integration test なので unwrap/expect を許可する。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! **Subagent Ask relay: a hardened Ask is queued for the MAIN session to adjudicate.**
//!
//! User rulings (verbatim):
//!
//! - 「sub agentのAskはmainで裁定する」
//! - 「この仕組みはコードで強制する、どうせわすれる」
//!
//! Inside a subagent blastguard hardens an Ask to a Deny. The new behaviour:
//! that hardened Ask is recorded in a relay queue
//! (`<BLASTGUARD_RELAY_DIR>/<session_id>.jsonl`, append-only JSONL with
//! `queued` / `executed` / `relayed` / `dismissed` events) so the main session
//! must re-run the exact command (where a human sees the Ask) and send the
//! result back with SendMessage; a main-session Stop hook (`blastguard relay
//! stop`) blocks until that is done or dismissed with a reason. The verdicts
//! themselves must not change (group (e) is GREEN on origin/main and must stay
//! GREEN).
//!
//! No test asserts prose: only decisions, JSON fields, ids, paths, commands and
//! exit codes. Every negative assertion ("not marked", "no queue") is paired
//! with a positive control in the same test so it cannot pass vacuously.

#[path = "support/bounded.rs"]
mod bounded;

use serde_json::{json, Value};
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

const ASK_CMD: &str = r#"for f in a b; do git checkout -- "$f"; done"#;
const SUB: &str = "ab0687739fbba8a8b";

struct Env {
    root: PathBuf,
    home: PathBuf,
    relay: PathBuf,
    approvals: PathBuf,
    cwd: PathBuf,
}

fn env(name: &str) -> Env {
    let root = std::env::temp_dir().join(format!("bg-relay-{}-{}", name, std::process::id()));
    let _ = std::fs::remove_dir_all(&root);
    let e = Env {
        home: root.join("home"),
        relay: root.join("relay"),
        approvals: root.join("approvals"),
        cwd: root.join("cwd"),
        root,
    };
    std::fs::create_dir_all(&e.home).unwrap();
    std::fs::create_dir_all(&e.approvals).unwrap();
    std::fs::create_dir_all(&e.cwd).unwrap();
    e
}

fn run(e: &Env, entrypoint: &str, args: &[&str], stdin: &str) -> Output {
    run_with_relay(e, &e.relay, entrypoint, args, stdin)
}

fn run_with_relay(e: &Env, relay: &Path, entrypoint: &str, args: &[&str], stdin: &str) -> Output {
    let mut c = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    c.args(args)
        .env("HOME", &e.home)
        .env("BLASTGUARD_RELAY_DIR", relay)
        .env("BLASTGUARD_APPROVALS_DIR", &e.approvals)
        .env("CLAUDE_CODE_SESSION_ID", "sessEnvIgnored")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", entrypoint)
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    bounded::run_bounded(&mut c, stdin.as_bytes(), bounded::HOOK_DEADLINE)
}

fn pre_payload(session: &str, command: &str, cwd: &Path, agent: Option<&str>) -> String {
    let mut v = json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
        "cwd": cwd.to_string_lossy(),
        "session_id": session,
    });
    if let Some(a) = agent {
        v["agent_id"] = json!(a);
        v["agent_type"] = json!("general-purpose");
    }
    v.to_string()
}

fn post_payload(
    event: &str,
    session: &str,
    command: &str,
    cwd: &Path,
    agent: Option<&str>,
) -> String {
    let mut v = json!({
        "hook_event_name": event,
        "tool_name": "Bash",
        "tool_input": { "command": command },
        "cwd": cwd.to_string_lossy(),
        "session_id": session,
        "tool_response": { "stdout": "x", "stderr": "", "interrupted": false, "exit_code": 0 },
    });
    if let Some(a) = agent {
        v["agent_id"] = json!(a);
        v["agent_type"] = json!("general-purpose");
    }
    v.to_string()
}

fn send_payload(session: &str, message: &str, agent: Option<&str>) -> String {
    let mut v = json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "SendMessage",
        "tool_input": { "to": "worker", "message": message },
        "session_id": session,
    });
    if let Some(a) = agent {
        v["agent_id"] = json!(a);
        v["agent_type"] = json!("general-purpose");
    }
    v.to_string()
}

fn stop_payload(session: &str, cwd: &Path, active: bool) -> String {
    json!({
        "hook_event_name": "Stop",
        "session_id": session,
        "cwd": cwd.to_string_lossy(),
        "stop_hook_active": active,
    })
    .to_string()
}

fn stdout_json(o: &Output) -> Option<Value> {
    let s = String::from_utf8_lossy(&o.stdout);
    if s.trim().is_empty() {
        return None;
    }
    serde_json::from_str(s.trim()).ok()
}

fn decision(o: &Output) -> String {
    match stdout_json(o) {
        None => "allow".to_string(),
        Some(d) => d["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or_else(|| panic!("no permissionDecision in {d}"))
            .to_string(),
    }
}

fn deny_reason(o: &Output) -> String {
    stdout_json(o).expect("stdout JSON")["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .expect("permissionDecisionReason")
        .to_string()
}

fn qfile(e: &Env, session: &str) -> PathBuf {
    e.relay.join(format!("{session}.jsonl"))
}

fn events(e: &Env, session: &str) -> Vec<Value> {
    match std::fs::read_to_string(qfile(e, session)) {
        Ok(s) => s
            .lines()
            .filter(|l| !l.trim().is_empty())
            .map(|l| serde_json::from_str(l).unwrap_or_else(|x| panic!("bad line {l}: {x}")))
            .collect(),
        Err(_) => vec![],
    }
}

fn of_kind(e: &Env, session: &str, kind: &str) -> Vec<Value> {
    events(e, session)
        .into_iter()
        .filter(|v| v["event"] == kind)
        .collect()
}

/// Subagent PreToolUse that must have queued; returns the id.
fn seed(e: &Env, session: &str, command: &str, cwd: &Path) -> String {
    let o = run(
        e,
        "cli",
        &[],
        &pre_payload(session, command, cwd, Some(SUB)),
    );
    assert_eq!(decision(&o), "deny");
    let q = of_kind(e, session, "queued");
    q.last()
        .unwrap_or_else(|| panic!("no queued line in {}", qfile(e, session).display()))["id"]
        .as_str()
        .unwrap()
        .to_string()
}

fn mark_executed(e: &Env, session: &str, command: &str, cwd: &Path, agent: Option<&str>) -> Output {
    run(
        e,
        "cli",
        &["relay", "mark-executed"],
        &post_payload("PostToolUse", session, command, cwd, agent),
    )
}

fn mark_relayed(e: &Env, session: &str, message: &str, agent: Option<&str>) -> Output {
    run(
        e,
        "cli",
        &["relay", "mark-relayed"],
        &send_payload(session, message, agent),
    )
}

fn stop(e: &Env, session: &str, active: bool) -> Output {
    run(
        e,
        "cli",
        &["relay", "stop"],
        &stop_payload(session, &e.cwd, active),
    )
}

fn blocks(o: &Output) -> bool {
    stdout_json(o).is_some_and(|d| d["decision"] == "block")
}

fn block_reason(o: &Output) -> String {
    stdout_json(o).expect("stdout JSON")["reason"]
        .as_str()
        .expect("reason")
        .to_string()
}

/// Positive control: a subagent Ask in a DIFFERENT session of the same relay
/// dir does queue, so "no queue" assertions elsewhere are not vacuous.
fn control_queue_works(e: &Env) {
    let id = seed(e, "sessControl", ASK_CMD, &e.cwd);
    assert!(id.starts_with("bgr-"));
}

// ---------------------------------------------------------------- (a)

#[test]
fn a_subagent_ask_is_denied_and_queued_once() {
    let e = env("a-queue");
    let o = run(
        &e,
        "cli",
        &[],
        &pre_payload("sessRelayA", ASK_CMD, &e.cwd, Some(SUB)),
    );
    assert_eq!(decision(&o), "deny");
    let q = of_kind(&e, "sessRelayA", "queued");
    assert_eq!(q.len(), 1, "events: {:?}", events(&e, "sessRelayA"));
    let q = &q[0];
    assert_eq!(q["command"], ASK_CMD);
    assert_eq!(q["cwd"], e.cwd.to_string_lossy().as_ref());
    assert_eq!(q["agent_id"], SUB);
    assert_eq!(q["session_id"], "sessRelayA");
    let id = q["id"].as_str().unwrap();
    assert_eq!(id.len(), 4 + 12);
    assert!(id.starts_with("bgr-"));
    assert!(id[4..].chars().all(|c| matches!(c, '0'..='9' | 'a'..='f')));
    assert!(q["ts"].is_i64() || q["ts"].is_u64());
    assert!(q["reason"].as_str().is_some_and(|r| !r.is_empty()));
    assert!(
        deny_reason(&o).contains(id),
        "deny reason must carry the relay id"
    );
}

#[test]
fn a_subagent_true_deny_is_not_queued() {
    let e = env("a-deny");
    for cmd in ["git reset --hard", "rm -rf /"] {
        let o = run(
            &e,
            "cli",
            &[],
            &pre_payload("sessRelayA", cmd, &e.cwd, Some(SUB)),
        );
        assert_eq!(decision(&o), "deny", "{cmd}");
    }
    assert!(of_kind(&e, "sessRelayA", "queued").is_empty());
    control_queue_works(&e);
}

#[test]
fn a_main_session_ask_is_not_queued() {
    let e = env("a-main");
    let o = run(
        &e,
        "cli",
        &[],
        &pre_payload("sessRelayA", ASK_CMD, &e.cwd, None),
    );
    assert_eq!(decision(&o), "ask");
    assert!(of_kind(&e, "sessRelayA", "queued").is_empty());
    control_queue_works(&e);
}

#[test]
fn a_subagent_ask_in_headless_env_is_not_queued() {
    let e = env("a-headless");
    let o = run(
        &e,
        "sdk-cli",
        &[],
        &pre_payload("sessRelayA", ASK_CMD, &e.cwd, Some(SUB)),
    );
    assert_eq!(decision(&o), "deny");
    assert!(of_kind(&e, "sessRelayA", "queued").is_empty());
    control_queue_works(&e);
}

#[test]
fn a_record_write_failure_is_surfaced_in_deny_reason() {
    let e = env("a-wfail");
    let not_a_dir = e.root.join("relay-is-a-file");
    std::fs::write(&not_a_dir, "x").unwrap();
    let o = run_with_relay(
        &e,
        &not_a_dir,
        "cli",
        &[],
        &pre_payload("sessRelayA", ASK_CMD, &e.cwd, Some(SUB)),
    );
    assert_eq!(decision(&o), "deny");
    assert!(
        deny_reason(&o).contains(not_a_dir.to_string_lossy().as_ref()),
        "reason must name the unwritable path"
    );
}

// ---------------------------------------------------------------- (b)

#[test]
fn b_mark_executed_exact_match_only() {
    let e = env("b-exec");
    let s = "sessRelayB";
    let id = seed(&e, s, ASK_CMD, &e.cwd);
    let other_cwd = e.root.join("elsewhere");
    std::fs::create_dir_all(&other_cwd).unwrap();
    let trailing = format!("{ASK_CMD} ");

    for o in [
        mark_executed(&e, s, ASK_CMD, &other_cwd, None),
        mark_executed(&e, s, &trailing, &e.cwd, None),
        mark_executed(&e, s, ASK_CMD, &e.cwd, Some(SUB)),
        mark_executed(&e, "sessOther", ASK_CMD, &e.cwd, None),
    ] {
        assert_eq!(o.status.code(), Some(0));
        assert!(
            String::from_utf8_lossy(&o.stdout).trim().is_empty(),
            "mark-executed must print no decision"
        );
    }
    assert!(
        of_kind(&e, s, "executed").is_empty(),
        "no mismatched call may mark executed: {:?}",
        events(&e, s)
    );
    assert!(of_kind(&e, "sessOther", "executed").is_empty());

    let o = mark_executed(&e, s, ASK_CMD, &e.cwd, None);
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).trim().is_empty());
    let x = of_kind(&e, s, "executed");
    assert_eq!(x.len(), 1, "{:?}", events(&e, s));
    assert_eq!(x[0]["id"], id.as_str());
    assert_eq!(x[0]["exit_code"], 0);
}

#[test]
fn b_mark_executed_also_for_post_tool_use_failure() {
    let e = env("b-fail");
    let s = "sessRelayB";
    let id = seed(&e, s, ASK_CMD, &e.cwd);
    let mut v: Value = serde_json::from_str(&post_payload(
        "PostToolUseFailure",
        s,
        ASK_CMD,
        &e.cwd,
        None,
    ))
    .unwrap();
    v["tool_response"] = json!({ "stdout": "", "stderr": "boom", "exit_code": 3 });
    let o = run(&e, "cli", &["relay", "mark-executed"], &v.to_string());
    assert_eq!(o.status.code(), Some(0));
    let x = of_kind(&e, s, "executed");
    assert_eq!(x.len(), 1, "{:?}", events(&e, s));
    assert_eq!(x[0]["id"], id.as_str());
    assert_eq!(x[0]["exit_code"], 3);
}

// ---------------------------------------------------------------- (c)

#[test]
fn c_mark_relayed_requires_executed_id_and_main_session() {
    let e = env("c-relay");
    let s = "sessRelayC";
    let id = seed(&e, s, ASK_CMD, &e.cwd);
    let msg = format!("result of {id}: ok");

    // Not yet executed -> not marked.
    let o = mark_relayed(&e, s, &msg, None);
    assert_eq!(o.status.code(), Some(0));
    assert!(of_kind(&e, s, "relayed").is_empty());

    mark_executed(&e, s, ASK_CMD, &e.cwd, None);
    assert_eq!(of_kind(&e, s, "executed").len(), 1, "{:?}", events(&e, s));

    // Executed, but wrong message / subagent sender -> not marked.
    mark_relayed(&e, s, "result without any id", None);
    mark_relayed(&e, s, &msg, Some(SUB));
    assert!(of_kind(&e, s, "relayed").is_empty(), "{:?}", events(&e, s));

    // Executed + main session + id mentioned -> marked.
    let o = mark_relayed(&e, s, &msg, None);
    assert_eq!(o.status.code(), Some(0));
    assert!(String::from_utf8_lossy(&o.stdout).trim().is_empty());
    let r = of_kind(&e, s, "relayed");
    assert_eq!(r.len(), 1, "{:?}", events(&e, s));
    assert_eq!(r[0]["id"], id.as_str());
}

// ---------------------------------------------------------------- (d)

#[test]
fn d_stop_blocks_on_pending_with_id_cwd_command() {
    let e = env("d-pending");
    let s = "sessRelayD";
    let id = seed(&e, s, ASK_CMD, &e.cwd);
    let o = stop(&e, s, false);
    assert_eq!(o.status.code(), Some(0));
    assert!(blocks(&o), "stdout: {}", String::from_utf8_lossy(&o.stdout));
    let r = block_reason(&o);
    assert!(r.contains(&id));
    assert!(r.contains(e.cwd.to_string_lossy().as_ref()));
    assert!(r.contains(ASK_CMD));
}

#[test]
fn d_stop_blocks_when_executed_only_and_allows_when_relayed() {
    let e = env("d-exec-relay");
    let s = "sessRelayD";
    let id = seed(&e, s, ASK_CMD, &e.cwd);
    mark_executed(&e, s, ASK_CMD, &e.cwd, None);
    assert_eq!(of_kind(&e, s, "executed").len(), 1);
    let o = stop(&e, s, false);
    assert!(blocks(&o), "executed-only must still block");
    assert!(block_reason(&o).contains(&id));

    mark_relayed(&e, s, &format!("see {id}"), None);
    assert_eq!(of_kind(&e, s, "relayed").len(), 1);
    let o = stop(&e, s, false);
    assert_eq!(o.status.code(), Some(0));
    assert!(
        !blocks(&o),
        "stdout: {}",
        String::from_utf8_lossy(&o.stdout)
    );
}

#[test]
fn d_dismiss_requires_reason_and_stays_visible() {
    let e = env("d-dismiss");
    let s = "sessRelayD";
    let id = seed(&e, s, ASK_CMD, &e.cwd);

    for args in [
        vec!["relay", "dismiss", id.as_str()],
        vec!["relay", "dismiss", id.as_str(), "--reason", ""],
        vec!["relay", "dismiss", id.as_str(), "--reason", "   "],
        vec!["relay", "dismiss", "bgr-000000000000", "--reason", "x"],
    ] {
        let o = run(&e, "cli", &args, "");
        assert_ne!(o.status.code(), Some(0), "{args:?} must fail");
    }
    assert!(of_kind(&e, s, "dismissed").is_empty());
    assert!(
        blocks(&stop(&e, s, false)),
        "still pending after bad dismisses"
    );

    let o = run(
        &e,
        "cli",
        &[
            "relay",
            "dismiss",
            id.as_str(),
            "--reason",
            "human declined",
        ],
        "",
    );
    assert_eq!(o.status.code(), Some(0));
    let d = of_kind(&e, s, "dismissed");
    assert_eq!(d.len(), 1);
    assert_eq!(d[0]["id"], id.as_str());
    assert_eq!(d[0]["reason"], "human declined");

    let o = stop(&e, s, false);
    assert_eq!(o.status.code(), Some(0));
    assert!(!blocks(&o));
    let doc = stdout_json(&o).expect("systemMessage JSON");
    let msg = doc["systemMessage"].as_str().expect("systemMessage");
    assert!(msg.contains(&id));
    assert!(msg.contains("human declined"));
}

#[test]
fn d_corrupt_queue_blocks_and_names_file() {
    let e = env("d-corrupt");
    let s = "sessRelayD";
    std::fs::create_dir_all(&e.relay).unwrap();
    std::fs::write(qfile(&e, s), "this is not json\n").unwrap();
    let o = stop(&e, s, false);
    assert!(blocks(&o), "stdout: {}", String::from_utf8_lossy(&o.stdout));
    assert!(block_reason(&o).contains(qfile(&e, s).to_string_lossy().as_ref()));
}

#[test]
fn d_unreadable_queue_blocks() {
    let e = env("d-unreadable");
    let s = "sessRelayD";
    std::fs::create_dir_all(qfile(&e, s)).unwrap();
    let o = stop(&e, s, false);
    assert!(blocks(&o), "stdout: {}", String::from_utf8_lossy(&o.stdout));
}

#[test]
fn d_no_queue_file_allows_but_queue_exists_blocks() {
    let e = env("d-nofile");
    let s = "sessRelayD";
    let o = stop(&e, s, false);
    assert_eq!(o.status.code(), Some(0));
    assert!(!blocks(&o));
    // Control: the same Stop invocation does block once something is pending.
    seed(&e, s, ASK_CMD, &e.cwd);
    assert!(blocks(&stop(&e, s, false)));
}

#[test]
fn d_stop_hook_active_still_blocks_pending() {
    let e = env("d-active");
    let s = "sessRelayD";
    seed(&e, s, ASK_CMD, &e.cwd);
    assert!(blocks(&stop(&e, s, true)));
}

#[test]
fn d_other_sessions_do_not_interfere() {
    let e = env("d-other");
    let idb = seed(&e, "sessRelayOther", ASK_CMD, &e.cwd);
    let ida = seed(&e, "sessRelayD", ASK_CMD, &e.cwd);
    mark_executed(&e, "sessRelayD", ASK_CMD, &e.cwd, None);
    mark_relayed(&e, "sessRelayD", &format!("{ida} done"), None);
    assert_eq!(of_kind(&e, "sessRelayD", "relayed").len(), 1);

    let o = stop(&e, "sessRelayD", false);
    assert!(!blocks(&o), "other session's pending must not block");
    let o = stop(&e, "sessRelayOther", false);
    assert!(blocks(&o));
    assert!(block_reason(&o).contains(&idb));
}

// ---------------------------------------------------------------- (e)

/// Verdicts measured on origin/main (unmodified binary) before this feature.
/// Env: CLAUDECODE=1, CLAUDE_CODE_ENTRYPOINT=cli, cwd = scratch dir.
#[test]
fn e_verdicts_unchanged() {
    let e = env("e-verdicts");
    // (command, main-session verdict, subagent verdict)
    let table: &[(&str, &str, &str)] = &[
        ("ls", "allow", "allow"),
        (ASK_CMD, "ask", "deny"),
        ("git reset --hard", "deny", "deny"),
        ("rm -rf /", "deny", "deny"),
        ("cat README.md", "allow", "allow"),
    ];
    for (cmd, main_v, sub_v) in table {
        let m = run(&e, "cli", &[], &pre_payload("sessE", cmd, &e.cwd, None));
        assert_eq!(&decision(&m), main_v, "main: {cmd}");
        let s = run(
            &e,
            "cli",
            &[],
            &pre_payload("sessE", cmd, &e.cwd, Some(SUB)),
        );
        assert_eq!(&decision(&s), sub_v, "subagent: {cmd}");
    }
}

#[test]
fn e_pretooluse_never_executes_the_command() {
    let e = env("e-noexec");
    let marker = e.root.join("MARKER");
    let cmd = format!("{ASK_CMD}; touch {}", marker.display());
    let o = run(
        &e,
        "cli",
        &[],
        &pre_payload("sessE", &cmd, &e.cwd, Some(SUB)),
    );
    assert_eq!(decision(&o), "deny");
    assert!(!marker.exists(), "the hook must never run the command");
    let o = run(&e, "cli", &[], &pre_payload("sessE", &cmd, &e.cwd, None));
    assert_eq!(decision(&o), "ask");
    assert!(!marker.exists());
}
