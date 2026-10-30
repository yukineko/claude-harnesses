#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `fugu-router agent-record` (SubagentStop) appends one class-"subagent"
//! USAGE episode per finished sub-agent, read from the sub-agent transcript.
//! The real SubagentStop payload carries no stop_reason (measured 2026-10-24),
//! so the episode is `outcome: "unscored"` and `pass: false`: usage only.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const TOOL_USE_ID: &str = "toolu_01ABC";

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

/// Lay out the real shape under `<home>/t/proj`: main transcript `S.jsonl`,
/// sub-agent transcript `S/subagents/agent-ab9f3a2ff61d54b06.jsonl` (when
/// `with_transcript`) and its sibling `.meta.json`. Returns
/// (main transcript, agent transcript path).
fn layout(home: &Path, with_transcript: bool, transcript: &str) -> (PathBuf, PathBuf) {
    let proj = home.join("t/proj");
    std::fs::create_dir_all(proj.join("S/subagents")).unwrap();
    let main_t = proj.join("S.jsonl");
    std::fs::write(&main_t, "").unwrap();
    let agent_t = proj.join("S/subagents/agent-ab9f3a2ff61d54b06.jsonl");
    if with_transcript {
        std::fs::write(&agent_t, transcript).unwrap();
        std::fs::write(
            proj.join("S/subagents/agent-ab9f3a2ff61d54b06.meta.json"),
            format!(
                r#"{{"agentType":"general-purpose","description":"D","toolUseId":"{TOOL_USE_ID}","model":"sonnet"}}"#
            ),
        )
        .unwrap();
    }
    (main_t, agent_t)
}

fn spawn(home: &Path, args: &[&str], payload: &str) -> (i32, String, String) {
    let mut child =
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_fugu-router")))
            .args(args)
            .env("HOME", home)
            .env_remove("FUGU_ROUTER_AUTO_MODEL")
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
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// The REAL SubagentStop payload shape (no stop_reason), fake paths.
fn stop_payload(main_t: &Path, agent_t: &Path, stop_hook_active: bool) -> String {
    serde_json::json!({
        "session_id": "c82ed0c9-0000-0000-0000-000000000000",
        "transcript_path": main_t,
        "cwd": "/tmp",
        "prompt_id": "dcb0171f-0000-0000-0000-000000000000",
        "permission_mode": "auto",
        "agent_id": "ab9f3a2ff61d54b06",
        "agent_type": "general-purpose",
        "effort": {"level": "medium"},
        "hook_event_name": "SubagentStop",
        "stop_hook_active": stop_hook_active,
        "agent_transcript_path": agent_t,
        "last_assistant_message": "done",
        "background_tasks": [],
        "session_crons": [],
    })
    .to_string()
}

fn pre_payload(tool_use_id: &str) -> String {
    serde_json::json!({
        "session_id": "c82ed0c9-0000-0000-0000-000000000000",
        "transcript_path": "/tmp/S.jsonl",
        "cwd": "/tmp",
        "prompt_id": "dcb0171f-0000-0000-0000-000000000000",
        "permission_mode": "auto",
        "effort": {"level": "medium"},
        "hook_event_name": "PreToolUse",
        "tool_name": "Agent",
        "tool_use_id": tool_use_id,
        "tool_input": {"description": "D", "prompt": "P", "subagent_type": "general-purpose"},
    })
    .to_string()
}

/// Condukt-population history so `agent-model` actually chooses and writes a marker.
fn seed_condukt(home: &Path) {
    std::fs::create_dir_all(home.join(".fugu-router")).unwrap();
    let mut s = String::new();
    for i in 0..5u64 {
        let ep = serde_json::json!({
            "ts": 1_700_000_000u64 + i, "title": "D\nP",
            "touched_files": ["agent:general-purpose"], "class": "parallel",
            "model": "sonnet", "role": "worker", "pass": true,
        });
        s.push_str(&ep.to_string());
        s.push('\n');
    }
    std::fs::write(home.join(".fugu-router/episodes.jsonl"), s).unwrap();
}

const TRANSCRIPT: &str = concat!(
    r#"{"type":"user","message":{"role":"user","content":"P"}}"#,
    "\n",
    r#"{"type":"assistant","message":{"role":"assistant","model":"claude-haiku-4-5","content":[{"type":"text","text":"x"}]}}"#,
    "\n",
    r#"{"type":"assistant","message":{"role":"assistant","model":"claude-sonnet-5-5","content":[{"type":"text","text":"y"}]}}"#,
    "\n",
);

fn assert_usage_episode(case: &str, v: &serde_json::Value) {
    assert_eq!(v["class"], "subagent", "case {case}: class; got {v}");
    assert_eq!(v["model"], "sonnet", "case {case}: model; got {v}");
    assert_eq!(
        v["model_id"], "claude-sonnet-5-5",
        "case {case}: model_id (last assistant line); got {v}"
    );
    assert_eq!(
        v["outcome"], "unscored",
        "case {case}: outcome must be unscored; got {v}"
    );
    assert_eq!(v["pass"], false, "case {case}: pass must be false; got {v}");
    assert_eq!(
        v["touched_files"],
        serde_json::json!(["agent:general-purpose"]),
        "case {case}: touched_files; got {v}"
    );
}

fn record_one(case: &str, stop_hook_active: bool) -> serde_json::Value {
    let home = temp_home(case);
    let (main_t, agent_t) = layout(&home, true, TRANSCRIPT);
    let (code, _o, stderr) = spawn(
        &home,
        &["agent-record"],
        &stop_payload(&main_t, &agent_t, stop_hook_active),
    );
    assert_eq!(code, 0, "case {case}: must exit 0; stderr={stderr}");
    let lines = store_lines(&home);
    assert_eq!(
        lines.len(),
        1,
        "case {case}: exactly one episode expected; stderr={stderr} store={lines:?}"
    );
    serde_json::from_str(&lines[0]).unwrap()
}

#[test]
fn e_real_payload_records_one_unscored_usage_episode() {
    let v = record_one("e", false);
    assert_usage_episode("e", &v);
}

#[test]
fn e4_stop_hook_active_true_still_records() {
    let v = record_one("e4", true);
    assert_usage_episode("e4", &v);
}

#[test]
fn e5_model_choice_is_the_agent_model_marker() {
    let home = temp_home("e5");
    seed_condukt(&home);
    let (main_t, agent_t) = layout(&home, true, TRANSCRIPT);
    // Produce the marker by really running agent-model with the meta toolUseId.
    let (pcode, pout, perr) = spawn(&home, &["agent-model"], &pre_payload(TOOL_USE_ID));
    assert_eq!(pcode, 0, "case e5: agent-model must exit 0; stderr={perr}");
    let pv: serde_json::Value = serde_json::from_str(pout.trim())
        .unwrap_or_else(|e| panic!("case e5: agent-model must choose ({e}): {pout:?}"));
    let chosen = pv["hookSpecificOutput"]["updatedInput"]["model"]
        .as_str()
        .unwrap()
        .to_string();
    let marker = home.join(".fugu-router/subagent-choice").join(TOOL_USE_ID);
    assert!(marker.is_file(), "case e5: marker missing at {marker:?}");
    let (code, _o, stderr) = spawn(
        &home,
        &["agent-record"],
        &stop_payload(&main_t, &agent_t, false),
    );
    assert_eq!(code, 0, "case e5: must exit 0; stderr={stderr}");
    let lines = store_lines(&home);
    let last: serde_json::Value = serde_json::from_str(lines.last().unwrap()).unwrap();
    assert_eq!(
        last["class"], "subagent",
        "case e5: last line is the usage episode; got {last}"
    );
    assert_eq!(
        last["model_choice"], chosen,
        "case e5: model_choice must be the marker value; got {last}"
    );
}

#[test]
fn e6_no_marker_model_choice_is_unknown() {
    let v = record_one("e6", false);
    assert_eq!(
        v["model_choice"], "unknown",
        "case e6: no marker -> model_choice unknown; got {v}"
    );
}

#[test]
fn f_missing_transcript_records_nothing() {
    let home = temp_home("f");
    let (main_t, agent_t) = layout(&home, false, "");
    let (code, _o, stderr) = spawn(
        &home,
        &["agent-record"],
        &stop_payload(&main_t, &agent_t, false),
    );
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
fn f2_unreadable_transcript_records_nothing() {
    let home = temp_home("f2");
    let (main_t, agent_t) = layout(&home, false, "");
    std::fs::create_dir_all(&agent_t).unwrap(); // a directory, not a file
    let (code, _o, stderr) = spawn(
        &home,
        &["agent-record"],
        &stop_payload(&main_t, &agent_t, false),
    );
    assert_eq!(code, 0, "case f2: must exit 0; stderr={stderr}");
    assert!(
        store_lines(&home).is_empty(),
        "case f2: unreadable transcript must record no episode"
    );
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "case f2: agent-record subcommand must exist; stderr={stderr}"
    );
}
