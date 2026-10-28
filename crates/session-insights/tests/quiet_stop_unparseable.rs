#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Quiet-stop item 3b: the Stop hook given an empty/unparseable payload prints
//! exactly ONE short `systemMessage` line on stdout (exit 0) and appends an `unparseable_payload` JSON line to
//! `<state_dir>/log.jsonl` (state_dir from Config::load of the cwd).

use std::io::Write;
use std::process::{Command, Stdio};

fn run_case(tag: &str, payload: &[u8]) {
    let home = std::env::temp_dir().join(format!(
        "si-quiet-{tag}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&home).unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_session-insights"))
        .arg("stop")
        .current_dir(&home)
        .env("HOME", &home)
        .env_remove("SESSION_INSIGHTS_DISABLE")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(payload).unwrap();
    let out = child.wait_with_output().unwrap();
    let log = home.join(".session-insights/state/log.jsonl");
    let text = std::fs::read_to_string(&log).unwrap_or_default();
    let _ = std::fs::remove_dir_all(&home);

    assert_eq!(out.status.code(), Some(0));
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(
        stdout.trim().lines().count(),
        1,
        "stdout must be exactly one line, got: {stdout:?}"
    );
    let msg: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON ({e}): {stdout:?}"));
    assert!(msg.get("decision").is_none(), "{msg}");
    assert_eq!(
        msg["systemMessage"], "session-insights: セッションを記録できませんでした",
        "stdout: {stdout:?}"
    );
    assert!(
        !stdout.contains("NOT counted"),
        "long message leaked: {stdout:?}"
    );
    assert!(
        !stdout.contains("not json"),
        "raw payload leaked: {stdout:?}"
    );
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!stderr.trim().is_empty(), "stderr disclosure must remain");
    let rec: serde_json::Value = serde_json::from_str(
        text.lines()
            .last()
            .unwrap_or_else(|| panic!("no log line at {}", log.display())),
    )
    .expect("log line is JSON");
    assert_eq!(rec["event"], "unparseable_payload");
    assert_eq!(rec["hook"], "stop");
    assert!(
        rec["message"]
            .as_str()
            .is_some_and(|m| m.contains("NOT counted")),
        "{rec}"
    );
}

#[test]
fn empty_payload_prints_one_line_and_is_logged() {
    run_case("empty", b"");
}

#[test]
fn garbage_payload_prints_one_line_and_is_logged() {
    run_case("garbage", b"not json");
}
