// Integration test: unwrap/expect/panic are fine here.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! GitHub issue #1341: `condukt editgate` allows silently when
//! `std::env::current_dir()` fails.
//!
//! `run_editgate` (crates/condukt/src/main.rs) does
//! `let cwd = match std::env::current_dir() { Ok(c) => c, Err(_) => return };`
//! so with a removed cwd the hook prints nothing and exits 0, which the
//! harness reads as "no block". Whether the edited Rust file is inside a live
//! condukt worktree is then unknown, and per CLAUDE.md section 3 that must
//! resolve to the restrictive side: a block (or ask) verdict on stdout, or a
//! non-zero exit with a stated reason.
//!
//! Written by the test author only (CLAUDE.md section 2(a)); no fix here.

use std::io::Write;
use std::process::{Command, Stdio};

const BIN: &str = env!("CARGO_BIN_EXE_condukt");

#[test]
fn editgate_with_deleted_cwd_does_not_silently_allow() {
    let home = tempfile::tempdir().unwrap();
    let base = tempfile::tempdir().unwrap();
    let gone = base.path().join("gone-cwd");
    std::fs::create_dir_all(&gone).unwrap();
    let target = home.path().join("src").join("lib.rs");
    std::fs::create_dir_all(target.parent().unwrap()).unwrap();
    std::fs::write(&target, "pub fn f() -> i32 { 1 }\n").unwrap();

    let payload = serde_json::json!({
        "hook_event_name": "PostToolUse",
        "tool_name": "Edit",
        "tool_input": { "file_path": target.to_string_lossy() },
    })
    .to_string();

    // Enter the directory, remove it, then exec condukt: the process starts
    // with a cwd that no longer exists, so `current_dir()` fails.
    let mut child = Command::new("/bin/sh")
        .arg("-c")
        .arg("cd \"$GONE\" && rmdir \"$GONE\" && exec \"$BIN\" editgate")
        .env("GONE", &gone)
        .env("BIN", BIN)
        .env("HOME", home.path())
        .env_remove("CONDUKT_DISABLE")
        .env_remove("CONDUKT_STATE_DIR")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn sh wrapper");
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        !gone.exists(),
        "fixture precondition: the cwd must have been removed"
    );

    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    let verdict_restrictive = stdout.lines().any(|l| {
        serde_json::from_str::<serde_json::Value>(l.trim())
            .ok()
            .and_then(|v| v.get("decision").and_then(|d| d.as_str()).map(String::from))
            .is_some_and(|d| d == "block" || d == "ask")
    });
    let failed_with_reason = !out.status.success() && !stderr.trim().is_empty();
    assert!(
        verdict_restrictive || failed_with_reason,
        "editgate with an undeterminable cwd must block/ask or exit non-zero with a reason, \
         not silently allow; exit={:?} stdout={stdout:?} stderr={stderr:?}",
        out.status.code()
    );
}
