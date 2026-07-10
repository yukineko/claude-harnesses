//! End-to-end tests: run the real built `harness-status` binary as a subcommand
//! CLI and assert on stdout + exit code. The binary is read-only over an
//! isolated temp HOME + CWD so it never touches the real home dir or a real
//! progress file, and `HARNESS_DATE` pins the clock for deterministic output.

use std::path::PathBuf;
use std::process::Command;

// `serde_json` is a regular (non-dev) dependency of this crate already, so it's
// available to integration tests without adding a dev-dependency.

/// A unique throwaway directory used as both HOME and CWD for one test.
fn temp_sandbox(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "harness-status-{tag}-{}-{:?}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// Run the binary with `args` in an isolated HOME/CWD. Returns (exit_code, stdout).
fn run(args: &[&str], tag: &str) -> (i32, String) {
    let bin = env!("CARGO_BIN_EXE_harness-status");
    let sandbox = temp_sandbox(tag);
    let out = Command::new(bin)
        .args(args)
        .current_dir(&sandbox)
        .env("HOME", &sandbox)
        .env("HARNESS_DATE", "2026-06-23")
        .output()
        .expect("binary runs");
    std::fs::remove_dir_all(&sandbox).ok();
    (
        out.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&out.stdout).into_owned(),
    )
}

#[test]
fn help_lists_subcommands() {
    let (code, stdout) = run(&["--help"], "help");
    assert_eq!(code, 0, "--help must exit 0");
    assert!(
        stdout.contains("Unified HOTL status"),
        "expected the about string, got: {stdout}"
    );
}

#[test]
fn budget_subcommand_reports_pinned_date() {
    // With an empty HOME there is no budget data, so today's spend is $0.0000.
    // The pinned HARNESS_DATE makes the line deterministic.
    let (code, stdout) = run(&["budget"], "budget");
    assert_eq!(code, 0, "budget must exit 0");
    assert!(
        stdout.contains("Today (2026-06-23):") && stdout.contains("session(s)"),
        "expected the budget summary line, got: {stdout}"
    );
}

#[test]
fn progress_subcommand_reports_missing_file() {
    // The sandbox CWD has no .claude/progress.md, so the no-progress branch fires.
    let (code, stdout) = run(&["progress"], "progress");
    assert_eq!(code, 0, "progress must exit 0");
    assert!(
        stdout.contains("[no progress file]"),
        "expected the missing-progress line, got: {stdout}"
    );
}

#[test]
fn session_start_is_silent_when_no_settings_json() {
    // No ~/.claude/settings.json in the fresh sandbox HOME → nothing to flag,
    // hook must stay completely silent (never break the turn with noise).
    let (code, stdout) = run(&["session-start"], "session-start-no-settings");
    assert_eq!(code, 0, "session-start must exit 0");
    assert!(
        stdout.trim().is_empty(),
        "expected no output when settings.json is absent, got: {stdout}"
    );
}

#[test]
fn session_start_is_silent_when_all_hook_binaries_present() {
    let sandbox = temp_sandbox("session-start-healthy");
    let claude_dir = sandbox.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    let present_bin = env!("CARGO_BIN_EXE_harness-status");
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [
                        {"type": "command", "command": format!("{present_bin} session-start")}
                    ]}
                ]
            }
        })
        .to_string(),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_harness-status");
    let out = std::process::Command::new(bin)
        .args(["session-start"])
        .current_dir(&sandbox)
        .env("HOME", &sandbox)
        .env("HARNESS_DATE", "2026-06-23")
        .output()
        .expect("binary runs");
    std::fs::remove_dir_all(&sandbox).ok();

    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "session-start must exit 0"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.trim().is_empty(),
        "expected no output when all hook binaries are present, got: {stdout}"
    );
}

#[test]
fn session_start_warns_via_additional_context_when_binary_missing() {
    let sandbox = temp_sandbox("session-start-missing");
    let claude_dir = sandbox.join(".claude");
    std::fs::create_dir_all(&claude_dir).unwrap();
    std::fs::write(
        claude_dir.join("settings.json"),
        serde_json::json!({
            "hooks": {
                "SessionStart": [
                    {"hooks": [
                        {"type": "command", "command": "/no/such/bin/ghost session-start"}
                    ]}
                ]
            }
        })
        .to_string(),
    )
    .unwrap();

    let bin = env!("CARGO_BIN_EXE_harness-status");
    let out = std::process::Command::new(bin)
        .args(["session-start"])
        .current_dir(&sandbox)
        .env("HOME", &sandbox)
        .env("HARNESS_DATE", "2026-06-23")
        .output()
        .expect("binary runs");
    std::fs::remove_dir_all(&sandbox).ok();

    assert_eq!(
        out.status.code().unwrap_or(-1),
        0,
        "session-start must exit 0 (fail-soft)"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let v: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("expected JSON additionalContext, got {stdout:?}: {e}"));
    let ctx = v["additionalContext"]
        .as_str()
        .expect("additionalContext must be a string");
    assert!(
        ctx.contains("/no/such/bin/ghost"),
        "expected warning to mention the missing binary path, got: {ctx}"
    );
}
