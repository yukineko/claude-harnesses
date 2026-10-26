#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Quiet-stop item 3a: autoflow's bounded allow (stop_hook_active=true, an
//! undetermined observation) prints NOTHING on stdout and appends a
//! `bounded_allow` JSON line to `<state_dir>/log.jsonl`. First stop still blocks.
//! Undetermined input: a payload with no session_id and no CLAUDE_CODE_SESSION_ID.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn home(tag: &str) -> PathBuf {
    let h = std::env::temp_dir().join(format!(
        "autoflow-quiet-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(h.join("repo/.git")).unwrap();
    h
}

fn stop(home: &PathBuf, active: bool) -> (i32, String) {
    let payload = format!(
        r#"{{"hook_event_name":"Stop","cwd":"{}","transcript_path":"","stop_hook_active":{active}}}"#,
        home.join("repo").to_string_lossy()
    );
    let mut child = Command::new(env!("CARGO_BIN_EXE_autoflow"))
        .arg("stop")
        .env("HOME", home)
        .env("PATH", "/usr/bin:/bin")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("AUTOFLOW_DISABLE")
        .current_dir(home.join("repo"))
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
    )
}

#[test]
fn first_stop_still_blocks() {
    let h = home("first");
    let (code, stdout) = stop(&h, false);
    let _ = std::fs::remove_dir_all(&h);
    assert_eq!(code, 0);
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).expect("block JSON");
    assert_eq!(v["decision"], "block");
}

#[test]
fn bounded_allow_is_silent_on_stdout_and_logged() {
    let h = home("bounded");
    let (code, stdout) = stop(&h, true);
    let log = h.join(".autoflow/state/log.jsonl");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&h);
    assert_eq!(code, 0);
    assert!(
        stdout.trim().is_empty(),
        "bounded allow must print nothing on stdout, got: {stdout:?}"
    );
    let rec: serde_json::Value = serde_json::from_str(
        text.lines()
            .last()
            .unwrap_or_else(|| panic!("no log line at {}", log.display())),
    )
    .expect("log line is JSON");
    assert_eq!(rec["event"], "bounded_allow");
    assert!(rec.get("session_id").is_some(), "{rec}");
    assert!(rec["what"].as_str().is_some_and(|s| !s.is_empty()), "{rec}");
    assert!(rec["why"].as_str().is_some_and(|s| !s.is_empty()), "{rec}");
}
