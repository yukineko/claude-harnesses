#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro: with block limits ARMED, an unmeasurable transcript or malformed
//! hook input makes `budgetguard gate` exit 0 with no decision and nothing on
//! stderr -- the stop is silently allowed.
//!
//! Cases (all with `[session] block_usd` and `[daily] block_usd` armed):
//!   (a) `transcript_path` does not exist
//!   (b) `transcript_path` is a directory
//!   (c) stdin is not JSON
//!   (d) `session_id` empty / `transcript_path` empty
//!
//! Suspected path: `gate_run` returns early on parse failure / empty fields
//! (`src/main.rs`), and `gate::session_cost` maps `estimate_transcript_cost`
//! returning `None` to `Determination::Known(None)`, which `evaluate` turns
//! into `return None` -> `emit_and_exit(None)` -> exit 0, silent. "Cannot
//! determine" is not "within budget": with limits armed the stop must be a
//! block decision (or at minimum a non-zero exit), never a silent allow.
//!
//! "Not silently allowed" is asserted as: stdout carries a block decision OR
//! the exit status is non-zero. Exit 0 with empty stdout is the bug.
//!
//! The `integration.rs` tests that pinned the allow for (c)/(d) without a
//! config were flipped by user ruling 2026-10-19 (now
//! `gate_with_malformed_stdin_blocks_even_with_no_limit` and
//! `gate_with_empty_fields_and_no_config_blocks`).

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn temp_home(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "budgetguard-repro-{}-{}-{}",
        tag,
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    // Arm both block limits via the project config in the gate's cwd.
    std::fs::write(
        dir.join("budgetguard.toml"),
        "[session]\nblock_usd = 1.0\n\n[daily]\nblock_usd = 1.0\n",
    )
    .unwrap();
    dir
}

/// Run `budgetguard gate` with `payload` on stdin; returns (exit, stdout, stderr).
fn gate(payload: &str, home: &Path) -> (i32, String, String) {
    let mut child = Command::new(env!("CARGO_BIN_EXE_budgetguard"))
        .arg("gate")
        .env("HOME", home)
        .env_remove("BUDGETGUARD_DISABLE")
        .current_dir(home)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

fn assert_not_silently_allowed(case: &str, code: i32, stdout: &str, stderr: &str) {
    let blocked = stdout.contains("\"block\"");
    assert!(
        blocked || code != 0,
        "{case}: limits are armed but the stop was silently allowed \
         (exit {code}, stdout {stdout:?}, stderr {stderr:?}); an unmeasurable \
         spend must resolve to block/undetermined, not to allow"
    );
}

#[test]
fn a_nonexistent_transcript_with_limits_armed_is_not_silently_allowed() {
    let home = temp_home("a");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "repro-a",
        "transcript_path": home.join("does-not-exist.jsonl"),
        "cwd": home,
    })
    .to_string();
    let (code, stdout, stderr) = gate(&payload, &home);
    assert_not_silently_allowed("(a) nonexistent transcript", code, &stdout, &stderr);
}

#[test]
fn b_directory_transcript_with_limits_armed_is_not_silently_allowed() {
    let home = temp_home("b");
    let dir = home.join("a-directory");
    std::fs::create_dir_all(&dir).unwrap();
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "repro-b",
        "transcript_path": dir,
        "cwd": home,
    })
    .to_string();
    let (code, stdout, stderr) = gate(&payload, &home);
    assert_not_silently_allowed("(b) directory transcript", code, &stdout, &stderr);
}

#[test]
fn c_non_json_stdin_with_limits_armed_is_not_silently_allowed() {
    let home = temp_home("c");
    let (code, stdout, stderr) = gate("not json", &home);
    assert_not_silently_allowed("(c) non-JSON stdin", code, &stdout, &stderr);
}

/// User ruling (2026-10-19): 「それは止めるで判断していい。読めないならエラー」.
/// Non-JSON stdin with a limit armed must BLOCK the stop, not merely exit
/// non-zero: Claude Code treats exit 1 as a non-blocking error and lets the
/// stop through. The block uses the gate's own Stop protocol — a
/// `{"decision":"block","reason":...}` line on stdout with exit 0.
fn assert_blocks_stop(case: &str, code: i32, stdout: &str, stderr: &str) {
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap_or_else(|e| {
        panic!("{case}: stdout is not a JSON decision ({e}); exit {code}, stdout {stdout:?}, stderr {stderr:?}")
    });
    assert_eq!(
        v["decision"], "block",
        "{case}: unparseable stdin must block the stop; got exit {code}, stdout {stdout:?}, stderr {stderr:?}"
    );
    assert!(
        v["reason"]
            .as_str()
            .is_some_and(|r| r.contains("Stop hook payload")),
        "{case}: the block reason must say the payload was unreadable: {v}"
    );
    assert_eq!(
        code, 0,
        "{case}: a Stop block decision is read only on exit 0"
    );
}

#[test]
fn c2_non_json_stdin_with_block_limit_blocks_the_stop() {
    let home = temp_home("c2");
    let (code, stdout, stderr) = gate("not json", &home);
    assert_blocks_stop("(c2) non-JSON stdin, block limit", code, &stdout, &stderr);
}

#[test]
fn c3_non_json_stdin_with_warn_only_limit_blocks_the_stop() {
    let home = temp_home("c3");
    std::fs::write(home.join("budgetguard.toml"), "[session]\nwarn_usd = 1.0\n").unwrap();
    let (code, stdout, stderr) = gate("not json", &home);
    assert_blocks_stop(
        "(c3) non-JSON stdin, warn-only limit",
        code,
        &stdout,
        &stderr,
    );
}

#[test]
fn c4_non_json_stdin_with_unreadable_config_blocks_the_stop() {
    let home = temp_home("c4");
    std::fs::write(home.join("budgetguard.toml"), "this is not toml\n").unwrap();
    let (code, stdout, stderr) = gate("not json", &home);
    assert_blocks_stop(
        "(c4) non-JSON stdin, unreadable config",
        code,
        &stdout,
        &stderr,
    );
}

/// Second user ruling (2026-10-19): 「あとはこれによってでる問題によっては変更するが、
/// 処理できないならエラーでいい」 — unparseable stdin blocks in ALL cases,
/// including when no limit is armed (no config file at all).
#[test]
fn c5_non_json_stdin_with_no_limit_blocks_the_stop() {
    let home = temp_home("c5");
    std::fs::remove_file(home.join("budgetguard.toml")).unwrap();
    let (code, stdout, stderr) = gate("not json", &home);
    assert_blocks_stop("(c5) non-JSON stdin, no limit", code, &stdout, &stderr);
}

#[test]
fn d_empty_session_or_transcript_with_limits_armed_is_not_silently_allowed() {
    let home = temp_home("d");
    let payload = serde_json::json!({
        "hook_event_name": "Stop",
        "session_id": "",
        "transcript_path": "",
        "cwd": home,
    })
    .to_string();
    let (code, stdout, stderr) = gate(&payload, &home);
    assert_not_silently_allowed("(d) empty fields", code, &stdout, &stderr);
}
