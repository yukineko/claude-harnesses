#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro for backlog 388737f1 (difflog half): the SessionStart hook payload
//! carries `transcript_path`, but `difflog::SessionState` persists only
//! session_id / start_sha / project / started_at, so a later consumer
//! (harness-recur M1, ab736bac) cannot get from a difflog session record to the
//! transcript that produced it.
//!
//! The harness_core::transcript ordered tool-event iterator half of the ticket
//! is an API that does not exist yet; its absence is a compile-time fact and is
//! not pinned here.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn scratch() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let d = std::env::temp_dir().join(format!(
        "difflog-backlog-388737f1-{}-{nanos}",
        std::process::id()
    ));
    std::fs::create_dir_all(d.join("home")).unwrap();
    std::fs::create_dir_all(d.join("repo")).unwrap();
    d
}

fn git(repo: &Path, args: &[&str]) {
    let o = Command::new("git")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {o:?}");
}

#[test]
#[ignore = "backlog 388737f1: open defect, remove ignore when fixed"]
fn backlog_388737f1_session_state_records_transcript_path() {
    let root = scratch();
    let repo = root.join("repo");
    git(&repo, &["init", "-q"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["commit", "-q", "--allow-empty", "-m", "init"]);
    let transcript = root.join("t.jsonl");
    std::fs::write(&transcript, "").unwrap();
    let payload = serde_json::json!({
        "session_id": "s388737f1",
        "hook_event_name": "SessionStart",
        "transcript_path": transcript,
        "cwd": repo,
    })
    .to_string();
    let mut child = Command::new(env!("CARGO_BIN_EXE_difflog"))
        .arg("session-start")
        .env("HOME", root.join("home"))
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .current_dir(&repo)
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
    let state_file = root.join("home/.difflog/logs/sessions/s388737f1.json");
    let state = std::fs::read_to_string(&state_file).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&root);
    // Anti-vacuity: the hook really wrote its session record.
    assert!(
        state.contains("start_sha"),
        "precondition: no session state written at {state_file:?}: {out:?}"
    );
    let v: serde_json::Value = serde_json::from_str(&state).unwrap();
    assert_eq!(
        v.get("transcript_path").and_then(|t| t.as_str()),
        Some(transcript.to_str().unwrap()),
        "SessionState does not carry the payload's transcript_path: {state}"
    );
}
