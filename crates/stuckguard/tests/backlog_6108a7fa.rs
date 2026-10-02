#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Repro for backlog 6108a7fa: stuckguard never reads the user's instruction
//! from the hook payload (no `transcript_path` / prompt read anywhere in
//! `src/`), so an operation the user EXPLICITLY asked to repeat is
//! indistinguishable from a stuck loop: the agent is told to "step back,
//! question its assumptions and try a different approach" for doing exactly
//! what it was asked.
//!
//! The test drives the real binary with PostToolUse payloads that carry the
//! standard `transcript_path` field, pointing at a transcript whose latest user
//! turn asks for the same command to be run three times in a row.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// The advice that only makes sense for an UNREQUESTED loop.
const STEP_BACK: &str = "一歩引いて前提を疑い";
const SESSION: &str = "backlog-6108a7fa";

fn home() -> tempfile::TempDir {
    let td = tempfile::tempdir().unwrap();
    std::fs::write(
        td.path().join("stuckguard.toml"),
        "repeat_threshold = 3\noscillation_threshold = 9\nescalate_after = 9\ncooldown_events = 0\n",
    )
    .unwrap();
    td
}

fn watch(home: &Path, transcript: Option<&Path>) -> String {
    let mut payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "session_id": SESSION,
        "tool_name": "Bash",
        "tool_input": {"command": "cargo test -p demo"},
    });
    if let Some(t) = transcript {
        payload["transcript_path"] = serde_json::json!(t.to_str().unwrap());
    }
    let mut child = Command::new(env!("CARGO_BIN_EXE_stuckguard"))
        .arg("watch")
        .current_dir(home)
        .env("HOME", home)
        .env("LESSONS_STORE_DIR", home.join("lessons"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn stuckguard");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.to_string().as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(out.status.code(), Some(0), "PostToolUse hook must exit 0");
    String::from_utf8_lossy(&out.stdout).into_owned()
}

fn instructed_transcript(dir: &Path) -> PathBuf {
    let p = dir.join("transcript.jsonl");
    let line = serde_json::json!({
        "type": "user",
        "message": {"role": "user",
                    "content": "Run `cargo test -p demo` three times in a row (flakiness check). \
                                Do not change anything between runs."}
    });
    std::fs::write(&p, format!("{line}\n")).unwrap();
    p
}

/// Anti-vacuity: with no instruction available, three identical calls do trip
/// the repeat detector and emit the step-back advice — so its absence below is
/// a real distinction, not a detector that never fires.
#[test]
fn control_unrequested_repetition_is_flagged() {
    let td = home();
    let outs: Vec<String> = (0..3).map(|_| watch(td.path(), None)).collect();
    assert!(
        outs[2].contains(STEP_BACK),
        "third identical call must trip: {outs:?}"
    );
}

#[test]
#[ignore = "backlog 6108a7fa: open defect, remove ignore when fixed"]
fn user_requested_repetition_is_not_treated_as_a_stuck_loop() {
    let td = home();
    let t = instructed_transcript(td.path());
    let outs: Vec<String> = (0..3).map(|_| watch(td.path(), Some(&t))).collect();
    assert!(
        !outs.iter().any(|o| o.contains(STEP_BACK)),
        "the user asked for exactly this repetition, yet stuckguard told the agent to \
         step back and try a different approach: {outs:?}"
    );
}
