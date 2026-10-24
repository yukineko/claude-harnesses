#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `fugu-router agent-record` (SubagentStop) appends one class-"subagent"
//! episode per finished sub-agent, read from the sub-agent transcript.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn temp_home(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fugu-router-subrec-{tag}-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn store_lines(home: &Path) -> Vec<String> {
    std::fs::read_to_string(home.join(".fugu-router/episodes.jsonl"))
        .unwrap_or_default()
        .lines()
        .filter(|l| !l.trim().is_empty())
        .map(str::to_string)
        .collect()
}

/// Lay out `<home>/t/proj/S.jsonl` and, if `with_transcript`, the sub-agent
/// transcript at `<home>/t/proj/S/subagents/agent-abc.jsonl` (+ meta).
fn layout(home: &Path, with_transcript: bool, transcript: &str) -> PathBuf {
    let proj = home.join("t/proj");
    std::fs::create_dir_all(proj.join("S/subagents")).unwrap();
    let main_t = proj.join("S.jsonl");
    std::fs::write(&main_t, "").unwrap();
    if with_transcript {
        std::fs::write(proj.join("S/subagents/agent-abc.jsonl"), transcript).unwrap();
        std::fs::write(
            proj.join("S/subagents/agent-abc.meta.json"),
            r#"{"agentType":"general-purpose","description":"D","toolUseId":"toolu_1"}"#,
        )
        .unwrap();
    }
    main_t
}

fn run(home: &Path, main_t: &Path, stop_reason: &str) -> (i32, String) {
    let payload = serde_json::json!({
        "session_id": "S",
        "hook_event_name": "SubagentStop",
        "transcript_path": main_t,
        "agent_id": "abc",
        "agent_type": "general-purpose",
        "stop_reason": stop_reason,
        "last_assistant_message": "done",
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .arg("agent-record")
        .env("HOME", home)
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

const TRANSCRIPT: &str = concat!(
    r#"{"type":"user","message":{"role":"user","content":"P"}}"#,
    "\n",
    r#"{"type":"assistant","message":{"role":"assistant","model":"claude-sonnet-5-5","content":[{"type":"text","text":"x"}]}}"#,
    "\n",
    r#"{"type":"assistant","message":{"role":"assistant","model":"claude-sonnet-5-5","content":[{"type":"text","text":"y"}]}}"#,
    "\n",
);

fn one_episode(case: &str, stop_reason: &str, want_pass: bool) {
    let home = temp_home(case);
    let main_t = layout(&home, true, TRANSCRIPT);
    let (code, stderr) = run(&home, &main_t, stop_reason);
    assert_eq!(code, 0, "case {case}: must exit 0; stderr={stderr}");
    let lines = store_lines(&home);
    assert_eq!(
        lines.len(),
        1,
        "case {case}: exactly one episode expected; stderr={stderr} store={lines:?}"
    );
    let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(v["class"], "subagent", "case {case}: class; got {v}");
    assert_eq!(v["model"], "sonnet", "case {case}: model; got {v}");
    assert_eq!(
        v["model_id"], "claude-sonnet-5-5",
        "case {case}: model_id; got {v}"
    );
    assert_eq!(v["pass"], want_pass, "case {case}: pass; got {v}");
}

#[test]
fn e_completed_records_one_passing_episode() {
    one_episode("e", "completed", true);
}

#[test]
fn e2_failed_records_non_passing_episode() {
    one_episode("e2-failed", "failed", false);
}

#[test]
fn e3_timeout_records_non_passing_episode() {
    one_episode("e3-timeout", "timeout", false);
}

#[test]
fn f_missing_transcript_records_nothing() {
    let home = temp_home("f");
    let main_t = layout(&home, false, "");
    let (code, stderr) = run(&home, &main_t, "completed");
    assert_eq!(code, 0, "case f: must exit 0; stderr={stderr}");
    assert!(
        store_lines(&home).is_empty(),
        "case f: missing transcript must record no episode"
    );
    // Vacuity guard: the subcommand must exist (not clap's usage error).
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "case f: agent-record subcommand must exist; stderr={stderr}"
    );
}

#[test]
fn h_user_interrupt_records_nothing() {
    let home = temp_home("h");
    let main_t = layout(&home, true, TRANSCRIPT);
    let (code, stderr) = run(&home, &main_t, "user_interrupt");
    assert_eq!(code, 0, "case h: must exit 0; stderr={stderr}");
    assert!(
        store_lines(&home).is_empty(),
        "case h: user_interrupt must record no episode"
    );
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "case h: agent-record subcommand must exist; stderr={stderr}"
    );
}
