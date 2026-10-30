#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Claude Code runs the SubagentStop hook BEFORE the sub-agent's final
//! assistant lines are flushed to `agent_transcript_path` (measured live
//! 2026-10-24: hook at 06:11:03.882Z, assistant line stamped 06:11:03.831Z
//! appeared only afterwards). `fugu-router agent-record` must therefore wait a
//! bounded time, re-reading until an assistant line with message.model shows up.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn temp_home(tag: &str) -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let dir = std::env::temp_dir().join(format!(
        "fugu-router-lateflush-{tag}-{}-{nanos}",
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

/// Returns (main transcript, agent transcript) with a user + attachment line only.
fn layout(home: &Path) -> (PathBuf, PathBuf) {
    let proj = home.join("t/proj");
    std::fs::create_dir_all(proj.join("S/subagents")).unwrap();
    let main_t = proj.join("S.jsonl");
    std::fs::write(&main_t, "").unwrap();
    let agent_t = proj.join("S/subagents/agent-ab9f3a2ff61d54b06.jsonl");
    std::fs::write(
        &agent_t,
        concat!(
            r#"{"type":"user","message":{"role":"user","content":"P"}}"#,
            "\n",
            r#"{"type":"attachment","attachment":{"type":"hook_success"}}"#,
            "\n",
        ),
    )
    .unwrap();
    std::fs::write(
        proj.join("S/subagents/agent-ab9f3a2ff61d54b06.meta.json"),
        r#"{"agentType":"general-purpose","description":"D","toolUseId":"toolu_01ABC","model":"sonnet"}"#,
    )
    .unwrap();
    (main_t, agent_t)
}

fn stop_payload(main_t: &Path, agent_t: &Path) -> String {
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
        "stop_hook_active": false,
        "agent_transcript_path": agent_t,
        "last_assistant_message": "done",
        "background_tasks": [],
        "session_crons": [],
    })
    .to_string()
}

fn spawn_child(home: &Path, payload: &str) -> std::process::Child {
    let mut child =
        harness_core::git_env::scrub(&mut Command::new(env!("CARGO_BIN_EXE_fugu-router")))
            .arg("agent-record")
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
    child
}

#[test]
fn late_flushed_assistant_line_is_recorded() {
    let home = temp_home("late");
    let (main_t, agent_t) = layout(&home);
    // Start signal: agent-record consumes this spawn-time marker immediately
    // before its first transcript read, so "marker gone" proves the process is
    // running and has (about to have) read the not-yet-flushed transcript.
    let marker_dir = home.join(".fugu-router/subagent-choice");
    std::fs::create_dir_all(&marker_dir).unwrap();
    let marker = marker_dir.join("toolu_01ABC");
    std::fs::write(&marker, "sonnet").unwrap();
    let child = spawn_child(&home, &stop_payload(&main_t, &agent_t));

    let t = agent_t.clone();
    let writer = std::thread::spawn(move || {
        let give_up = Instant::now() + Duration::from_secs(8);
        while marker.exists() && Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(!marker.exists(), "agent-record never consumed the marker");
        std::thread::sleep(Duration::from_millis(400));
        let mut f = std::fs::OpenOptions::new().append(true).open(&t).unwrap();
        f.write_all(
            concat!(
                r#"{"type":"assistant","message":{"role":"assistant","model":"claude-sonnet-5-5","content":[{"type":"text","text":"ok"}]}}"#,
                "\n"
            )
            .as_bytes(),
        )
        .unwrap();
    });

    let out = child.wait_with_output().unwrap();
    writer.join().unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(0), "must exit 0; stderr={stderr}");
    let lines = store_lines(&home);
    assert_eq!(
        lines.len(),
        1,
        "exactly one episode expected after the late flush; stderr={stderr} store={lines:?}"
    );
    let v: serde_json::Value = serde_json::from_str(&lines[0]).unwrap();
    assert_eq!(v["class"], "subagent", "class; got {v}");
    assert_eq!(v["model"], "sonnet", "model; got {v}");
    assert_eq!(v["model_id"], "claude-sonnet-5-5", "model_id; got {v}");
    assert_eq!(v["outcome"], "unscored", "outcome; got {v}");
}

#[test]
fn never_flushed_records_nothing_within_bound() {
    let home = temp_home("never");
    let (main_t, agent_t) = layout(&home);
    let start = Instant::now();
    let child = spawn_child(&home, &stop_payload(&main_t, &agent_t));
    let out = child.wait_with_output().unwrap();
    let elapsed = start.elapsed();
    let stderr = String::from_utf8_lossy(&out.stderr).into_owned();
    assert_eq!(out.status.code(), Some(0), "must exit 0; stderr={stderr}");
    assert!(
        store_lines(&home).is_empty(),
        "no assistant line ever arrived: nothing must be recorded"
    );
    assert!(
        elapsed < Duration::from_secs(9),
        "wait must be bounded well under the 10 s hook timeout; took {elapsed:?}"
    );
    assert!(
        !stderr.contains("unrecognized subcommand"),
        "agent-record subcommand must exist; stderr={stderr}"
    );
}
