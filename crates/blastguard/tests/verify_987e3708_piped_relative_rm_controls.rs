#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Controls for backlog 987e3708: a relative, non-system `rm` target read from
//! `echo ... | bash` is judged like the same line written directly (not
//! stricter), while the system-harm variants stay Deny. Each case runs with a
//! fresh HOME so the repeat-deny downgrade cannot change a verdict.
//!
//! DOES NOT PROVE: shapes other than the listed `echo ... | bash` lines.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::Command;

fn decision(tag: &str, command: &str) -> (String, String) {
    // Relative-target cases need a cwd that is not itself a system directory:
    // on macOS `std::env::temp_dir()` is under `/private/var`, a system prefix,
    // where plain `rm -rf ./build` is Deny too (observed 2026-11-21).
    let base = if tag == "home" {
        std::env::temp_dir()
    } else {
        std::path::PathBuf::from("/tmp")
    };
    let home = base.join(format!("blastguard-987e3708-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(home.join("wt")).expect("mkdir home");
    let payload = serde_json::json!({
        "session_id": "backlog-987e3708-controls",
        "cwd": home.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env("HOME", &home)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID");
    cmd.env("PATH", "/usr/bin:/bin");
    let out = bounded::run_bounded(&mut cmd, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    let _ = std::fs::remove_dir_all(&home);
    if stdout.trim().is_empty() {
        eprintln!("[{tag}] allow");
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let d = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let r = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    eprintln!("[{tag}] {d}: {r}");
    (d, r)
}

fn rank(d: &str) -> u8 {
    match d {
        "allow" => 0,
        "ask" => 1,
        _ => 2,
    }
}

#[test]
fn piped_relative_rm_is_not_stricter_than_plain() {
    let (plain, pr) = decision("plain", "rm -rf ./build");
    let (piped, r) = decision("piped", "echo rm -rf ./build | bash");
    assert!(
        rank(&piped) <= rank(&plain),
        "plain={plain} ({pr}) piped={piped} ({r})"
    );
}

#[test]
fn piped_relative_rm_is_not_deny() {
    let (d, r) = decision("notdeny", "echo rm -rf ./build | bash");
    assert_ne!(d, "deny", "{r}");
}

#[test]
fn piped_rm_etc_stays_deny() {
    let (d, r) = decision("etc", "echo rm -rf /etc | bash");
    assert_eq!(d, "deny", "{r}");
}

#[test]
fn piped_rm_home_stays_deny() {
    let (d, r) = decision("home", "echo rm -rf ~ | bash");
    assert_eq!(d, "deny", "{r}");
}
