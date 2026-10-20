// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! User ruling 2026-10-19: 「retry3回でぬけだす」.
//!
//! Unparseable Stop stdin blocks the stop, but at most 3 consecutive times:
//! the 4th consecutive unparseable stop is allowed with a loud
//! `systemMessage` saying the gate gave up. The streak lives in a marker under
//! budgetguard's state_dir, keyed by `CLAUDE_CODE_SESSION_ID` (else one global
//! key); a parsed stop clears it. A marker that cannot be read or written
//! allows loudly.

use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

fn home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "budgetguard-retry-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(
        dir.join("budgetguard.toml"),
        "[daily]\nblock_usd = 1000.0\n",
    )
    .unwrap();
    dir
}

fn gate(home: &PathBuf, stdin: &str, session: Option<&str>) -> serde_json::Value {
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_budgetguard"));
    cmd.arg("gate")
        .env("HOME", home)
        .env_remove("BUDGETGUARD_DISABLE")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if let Some(s) = session {
        cmd.env("CLAUDE_CODE_SESSION_ID", s);
    }
    let mut child = cmd.spawn().unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(stdin.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert_eq!(out.status.code(), Some(0), "stdout {stdout:?}");
    if stdout.trim().is_empty() {
        return serde_json::Value::Null;
    }
    serde_json::from_str(stdout.trim()).unwrap_or_else(|e| panic!("not JSON ({e}): {stdout:?}"))
}

fn assert_block(v: &serde_json::Value, n: usize) {
    assert_eq!(
        v["decision"], "block",
        "unparseable stop #{n} must block: {v}"
    );
}

fn assert_loud_allow(v: &serde_json::Value, what: &str) {
    assert!(v.get("decision").is_none(), "{what}: must allow, got {v}");
    let msg = v["systemMessage"]
        .as_str()
        .unwrap_or_else(|| panic!("{what}: no systemMessage: {v}"));
    assert!(msg.contains("budgetguard"), "{what}: {msg}");
}

fn parsed_stop(home: &PathBuf) -> String {
    // A transcript with nothing to price: a known zero, allowed.
    let t = home.join("empty.jsonl");
    std::fs::write(&t, "").unwrap();
    serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "retry-s",
        "transcript_path": t,
        "cwd": home,
    })
    .to_string()
}

#[test]
fn blocks_three_times_then_allows_the_fourth_with_a_session() {
    let h = home("sess");
    for n in 1..=3 {
        assert_block(&gate(&h, "not json", Some("sess-a")), n);
    }
    let v = gate(&h, "not json", Some("sess-a"));
    assert_loud_allow(&v, "4th unparseable stop");
    assert!(v["systemMessage"].as_str().unwrap().contains('3'), "{v}");
}

#[test]
fn blocks_three_times_then_allows_the_fourth_without_a_session() {
    let h = home("global");
    for n in 1..=3 {
        assert_block(&gate(&h, "not json", None), n);
    }
    assert_loud_allow(
        &gate(&h, "not json", None),
        "4th unparseable stop (global key)",
    );
}

#[test]
fn the_streak_is_per_session() {
    let h = home("per");
    for n in 1..=3 {
        assert_block(&gate(&h, "not json", Some("sess-a")), n);
    }
    // A different session starts its own streak.
    assert_block(&gate(&h, "not json", Some("sess-b")), 1);
}

#[test]
fn a_parsed_stop_resets_the_streak() {
    let h = home("reset");
    for n in 1..=2 {
        assert_block(&gate(&h, "not json", Some("sess-r")), n);
    }
    let ok = gate(&h, &parsed_stop(&h), Some("sess-r"));
    assert!(
        ok.get("decision").is_none(),
        "parsed stop under budget: {ok}"
    );
    for n in 1..=3 {
        assert_block(&gate(&h, "not json", Some("sess-r")), n);
    }
    assert_loud_allow(&gate(&h, "not json", Some("sess-r")), "4th after reset");
}

#[test]
fn an_unusable_marker_allows_loudly() {
    let h = home("io");
    let state = h.join(".budgetguard").join("state");
    std::fs::create_dir_all(&state).unwrap();
    // A regular file where the streak directory must be: the marker can be
    // neither read nor written.
    std::fs::write(state.join("unparseable-streak"), "x").unwrap();
    assert_loud_allow(&gate(&h, "not json", Some("sess-io")), "marker IO failure");
}
