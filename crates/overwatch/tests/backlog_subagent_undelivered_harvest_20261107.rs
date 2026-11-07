// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro (2026-11-07, session c2e3bc17): an agent that ended WITHOUT
//! delivering its report (SubagentStop fired, final assistant text sits in its
//! transcript, no SubagentHandback success) must be classified
//! `ended_undelivered` by `overwatch subagents --json`, and its final text
//! must be HARVESTED into the durable registry record (field `final_text`) so
//! the report survives transcript deletion.
//!
//! Payload shapes follow real captures (PostToolUse Agent async_launched with
//! `tool_response.agentId`; SubagentStop with `agent_id`, `agent_type`,
//! `agent_transcript_path`, `last_assistant_message`, `background_tasks`,
//! `stop_hook_active`, `cwd`).
//!
//! Only classification, JSON fields and exit codes are asserted (user ruling
//! 2026-11-07: no tests on prose). Written by a disinterested author
//! (CLAUDE.md section 2(a)): no fix exists. Companion of
//! backlog_subagent_liveness_20261107.rs (helpers copied, not shared).

use serde_json::{json, Value};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::time::{SystemTime, UNIX_EPOCH};

const BIN: &str = env!("CARGO_BIN_EXE_overwatch");
const SESSION: &str = "11111111-2222-3333-4444-555555555555";
const STALL_SECS: i64 = 600;
const UNDELIVERED: &str = "b0000000000000001";
const DELIVERED: &str = "b0000000000000002";
const FINAL: &str = "Verification report: all 3 checks pass; nothing to fix.";
const STOP_MSG: &str = "Stop-payload report: sentinel-7f3a";

fn now() -> i64 {
    static N: OnceLock<i64> = OnceLock::new();
    *N.get_or_init(|| {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs() as i64
    })
}

/// `now() + off` as RFC 3339 UTC (the transcript timestamp format).
fn ts(off: i64) -> String {
    let t = now() + off;
    let days = t.div_euclid(86_400);
    let secs = t.rem_euclid(86_400);
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}-{m:02}-{d:02}T{:02}:{:02}:{:02}.000Z",
        secs / 3600,
        (secs / 60) % 60,
        secs % 60
    )
}

fn jl(entries: &[Value]) -> String {
    let mut s = String::new();
    for e in entries {
        s.push_str(&e.to_string());
        s.push('\n');
    }
    s
}

fn tool_use(off: i64, id: &str, name: &str, input: Value) -> Value {
    json!({"type": "assistant", "timestamp": ts(off), "isSidechain": true,
        "message": {"role": "assistant", "content": [
            {"type": "tool_use", "id": id, "name": name, "input": input}]}})
}

fn tool_result(off: i64, id: &str, content: &str) -> Value {
    json!({"type": "user", "timestamp": ts(off), "isSidechain": true,
        "message": {"role": "user", "content": [
            {"type": "tool_result", "tool_use_id": id, "content": content}]}})
}

fn text(off: i64, t: &str) -> Value {
    json!({"type": "assistant", "timestamp": ts(off), "isSidechain": true,
        "message": {"role": "assistant", "content": [{"type": "text", "text": t}]}})
}

fn bash(off: i64, id: &str, cmd: &str, out: &str) -> Vec<Value> {
    vec![
        tool_use(off, id, "Bash", json!({"command": cmd})),
        tool_result(off + 1, id, out),
    ]
}

fn handback(off: i64) -> Vec<Value> {
    vec![
        tool_use(
            off,
            "toolu_hb",
            "SubagentHandback",
            json!({"message": "report"}),
        ),
        tool_result(
            off + 1,
            "toolu_hb",
            "{\"success\":true,\"message\":\"Report delivered to your caller.\"}",
        ),
        text(off + 2, "Done."),
    ]
}

fn slug(p: &Path) -> String {
    p.to_string_lossy()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

struct Fx {
    _t: tempfile::TempDir,
    home: PathBuf,
    project: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let home = root.join("home");
        let project = root.join("proj");
        std::fs::create_dir_all(&home).unwrap();
        std::fs::create_dir_all(&project).unwrap();
        Fx {
            _t: t,
            home,
            project,
        }
    }
    fn proj_dir(&self) -> PathBuf {
        self.home.join(".claude/projects").join(slug(&self.project))
    }
    fn parent_path(&self, session: &str) -> PathBuf {
        self.proj_dir().join(format!("{session}.jsonl"))
    }
    fn agent_path(&self, session: &str, id: &str) -> PathBuf {
        self.proj_dir()
            .join(session)
            .join("subagents")
            .join(format!("agent-{id}.jsonl"))
    }
    fn parent(&self, session: &str, entries: &[Value]) {
        let p = self.parent_path(session);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, jl(entries)).unwrap();
    }
    fn transcript(&self, session: &str, id: &str, entries: &[Value]) {
        let p = self.agent_path(session, id);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, jl(entries)).unwrap();
    }

    /// Feed one hook payload to `overwatch presence` (the RECORD side).
    fn hook(&self, payload: Value) {
        let mut child = harness_core::git_env::scrub(&mut Command::new(BIN))
            .arg("presence")
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(payload.to_string().as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(
            out.status.success(),
            "presence hook failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// PostToolUse of an async Agent launch from the main thread.
    fn launch(&self, session: &str, n: usize, agent: &str) {
        self.hook(json!({
            "session_id": session,
            "transcript_path": self.parent_path(session).to_string_lossy(),
            "cwd": self.project.to_string_lossy(),
            "hook_event_name": "PostToolUse",
            "tool_name": "Agent",
            "tool_input": {"description": format!("task {n}"), "prompt": "work",
                "subagent_type": "general-purpose", "run_in_background": true},
            "tool_response": {"isAsync": true, "status": "async_launched",
                "agentId": agent, "description": format!("task {n}")},
            "tool_use_id": format!("toolu_launch_{n:04}")
        }));
    }

    /// SubagentStop in the observed real shape; `last` is the optional
    /// `last_assistant_message`.
    fn subagent_stop(&self, session: &str, agent: &str, last: Option<&str>) {
        let mut p = json!({
            "session_id": session,
            "transcript_path": self.parent_path(session).to_string_lossy(),
            "cwd": self.project.to_string_lossy(),
            "hook_event_name": "SubagentStop",
            "stop_hook_active": false,
            "agent_id": agent,
            "agent_type": "general-purpose",
            "agent_transcript_path": self.agent_path(session, agent).to_string_lossy(),
            "background_tasks": [],
            "session_crons": []
        });
        if let Some(l) = last {
            p["last_assistant_message"] = json!(l);
        }
        self.hook(p);
    }

    /// The CHECK side.
    fn check(&self, session: &str) -> (Option<i32>, Option<Value>) {
        let out = harness_core::git_env::scrub(&mut Command::new(BIN))
            .args([
                "subagents",
                "--session",
                session,
                "--stall-secs",
                &STALL_SECS.to_string(),
                "--json",
            ])
            .current_dir(&self.project)
            .env("HOME", &self.home)
            .output()
            .expect("spawn overwatch");
        eprintln!(
            "exit={:?}\nstdout={}\nstderr={}",
            out.status.code(),
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        );
        (
            out.status.code(),
            serde_json::from_slice::<Value>(&out.stdout).ok(),
        )
    }

    /// Agent that ran a tool, wrote a final text report, then stopped without
    /// SubagentHandback. Stop payload carries `last` as last_assistant_message.
    fn undelivered(&self, last: Option<&str>) {
        self.parent(SESSION, &[text(-3000, "dispatching")]);
        self.launch(SESSION, 1, UNDELIVERED);
        let mut a = bash(-1740, "toolu_n1", "cargo test -q", "3 passed");
        a.push(text(-1680, FINAL));
        self.transcript(SESSION, UNDELIVERED, &a);
        self.subagent_stop(SESSION, UNDELIVERED, last);
    }
}

fn entry<'a>(v: &'a Value, id: &str) -> &'a Value {
    v.get("agents")
        .and_then(Value::as_array)
        .expect("`agents` array in --json output")
        .iter()
        .find(|a| a.get("agent_id").and_then(Value::as_str) == Some(id))
        .unwrap_or_else(|| panic!("agent {id} not listed in {v}"))
}

fn state<'a>(v: &'a Value, id: &str) -> &'a str {
    entry(v, id)
        .get("state")
        .and_then(Value::as_str)
        .expect("state")
}

/// Control (passes today): the RECORD hook accepts the real-shaped launch and
/// SubagentStop payloads (incl. last_assistant_message) without failing, so
/// the RED tests below fail on the CHECK side, not on fixture rejection.
#[test]
fn control_record_hook_accepts_real_shaped_payloads() {
    let fx = Fx::new();
    fx.undelivered(Some(STOP_MSG));
}

#[test]
fn stop_without_handback_is_ended_undelivered_with_final_text() {
    let fx = Fx::new();
    fx.undelivered(None);

    let (code, v) = fx.check(SESSION);
    let v = v.expect("--json output must parse");
    assert_eq!(state(&v, UNDELIVERED), "ended_undelivered");
    assert_eq!(
        entry(&v, UNDELIVERED).get("final_text"),
        Some(&json!(FINAL)),
        "final_text must equal the transcript's last assistant text"
    );
    assert_eq!(v.get("clean"), Some(&json!(false)));
    assert_eq!(code, Some(1));
}

#[test]
fn harvested_final_text_survives_transcript_deletion() {
    let fx = Fx::new();
    fx.undelivered(None);

    let (_, v1) = fx.check(SESSION);
    let v1 = v1.expect("first --json output must parse");
    assert_eq!(state(&v1, UNDELIVERED), "ended_undelivered");

    std::fs::remove_file(fx.agent_path(SESSION, UNDELIVERED)).unwrap();

    let (code, v2) = fx.check(SESSION);
    let v2 = v2.expect("second --json output must parse");
    assert_eq!(
        state(&v2, UNDELIVERED),
        "ended_undelivered",
        "must not degrade to undetermined/missing once the transcript is gone"
    );
    assert_eq!(
        entry(&v2, UNDELIVERED).get("final_text"),
        Some(&json!(FINAL)),
        "final_text must come from the durable registry record"
    );
    assert_eq!(v2.get("clean"), Some(&json!(false)));
    assert_eq!(code, Some(1));
}

#[test]
fn missing_transcript_uses_stop_payload_last_assistant_message() {
    let fx = Fx::new();
    fx.undelivered(Some(STOP_MSG));
    std::fs::remove_file(fx.agent_path(SESSION, UNDELIVERED)).unwrap();

    let (code, v) = fx.check(SESSION);
    let v = v.expect("--json output must parse");
    assert_eq!(state(&v, UNDELIVERED), "ended_undelivered");
    assert_eq!(
        entry(&v, UNDELIVERED).get("final_text"),
        Some(&json!(STOP_MSG))
    );
    assert_eq!(v.get("clean"), Some(&json!(false)));
    assert_eq!(code, Some(1));
}

/// Control direction: a delivered result (SubagentHandback success) is `done`,
/// never `ended_undelivered`, and the session is clean.
#[test]
fn control_handback_success_is_done_not_ended_undelivered() {
    let fx = Fx::new();
    fx.parent(SESSION, &[text(-3000, "dispatching")]);
    fx.launch(SESSION, 2, DELIVERED);
    let mut a = bash(-1740, "toolu_d1", "cargo test -q", "ok");
    a.extend(handback(-1700));
    fx.transcript(SESSION, DELIVERED, &a);
    fx.subagent_stop(SESSION, DELIVERED, Some("Done."));

    let (code, v) = fx.check(SESSION);
    let v = v.expect("--json output must parse");
    assert_eq!(state(&v, DELIVERED), "done");
    assert_eq!(v.get("clean"), Some(&json!(true)));
    assert_eq!(code, Some(0));
}
