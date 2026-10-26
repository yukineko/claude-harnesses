#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! The UserPromptSubmit `prompt` hook injects a "routing episode(s) —
//! per-model outcomes" summary. Class-"subagent" episodes are a separate
//! population (recorded by the SubagentStop hook) and must not be counted in
//! that condukt routing summary.

use std::io::Write;
use std::process::{Command, Stdio};

#[test]
fn prompt_summary_counts_only_condukt_episodes() {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let home = std::env::temp_dir().join(format!(
        "fugu-router-promptsummary-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(home.join(".fugu-router")).unwrap();
    let mut s = String::new();
    for i in 0..3u64 {
        s.push_str(
            &serde_json::json!({"ts": 1_700_000_000u64 + i, "title": "D\nP",
                "touched_files": ["agent:general-purpose"], "class": "subagent",
                "model": "haiku", "role": "worker", "pass": false})
            .to_string(),
        );
        s.push('\n');
    }
    s.push_str(
        &serde_json::json!({"ts": 1_700_000_100u64, "title": "add a login feature",
            "touched_files": ["src/api.rs"], "class": "parallel",
            "model": "sonnet", "role": "worker", "pass": true})
        .to_string(),
    );
    s.push('\n');
    std::fs::write(home.join(".fugu-router").join("episodes.jsonl"), s).unwrap();

    let mut child = Command::new(env!("CARGO_BIN_EXE_fugu-router"))
        .arg("prompt")
        .env("HOME", &home)
        .env_remove("FUGU_ROUTER_DISABLED")
        .current_dir(&home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(
            br#"{"hook_event_name":"UserPromptSubmit","prompt":"add a login feature to the api"}"#,
        )
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(out.status.code(), Some(0), "prompt hook must exit 0");
    assert!(
        stdout.contains("1 routing episode(s)"),
        "summary must count only the 1 condukt episode, not the 3 subagent ones; got {stdout}"
    );
    assert!(
        !stdout.contains("haiku"),
        "subagent-class haiku outcomes leaked into the condukt routing summary; got {stdout}"
    );
}
