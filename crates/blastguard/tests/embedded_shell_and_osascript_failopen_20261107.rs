// このファイルは丸ごと integration test なので unwrap/expect/panic を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED repro of two blastguard fail-opens found 2026-11-07 by a differential
//! probe (session 0d3d5bb8). Both were Allow on main and were Allow before the
//! latest interpreter-heredoc change too.
//!
//! User rulings 2026-11-07: any command that harms the system is Deny through
//! any wrapper (this is a critical bug), and false positives are not
//! acceptable.
//!
//! 1. A `python3 - <<'PY'` body that calls `os.system('echo x >
//!    .git/hooks/pre-commit')` (an embedded shell string writing a protected
//!    gate path) is Allow; the same redirect at top level is Deny.
//! 2. `osascript -e 'tell app "Finder" to delete ...'` is Allow.
//!
//! Targets assert Deny. Controls (harmless embedded shell / harmless
//! osascript) must stay non-Deny, and the top-level redirect must stay Deny.
//! Payloads carry `agent_id` (subagent context). Written by a disinterested
//! author (CLAUDE.md section 2(a)); `src/` untouched.

#[path = "support/bounded.rs"]
mod bounded;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

fn fresh_dir(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "blastguard-embedded-osa-{}-{tag}",
        std::process::id()
    ));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("mkdir");
    dir
}

/// (decision, reason) for `command` judged as a subagent Bash call.
/// Empty stdout is Allow.
fn verdict(cwd: &Path, command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "cwd": cwd.to_string_lossy(),
        "agent_id": "test-subagent",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.current_dir(cwd)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .env_remove("BLASTGUARD_ASK")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(
        out.status.code(),
        Some(0),
        "hook must exit 0 (cmd: {command:?}); stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout is not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}, got: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

#[track_caller]
fn assert_all_deny(prefix: &str, cmds: &[&str]) {
    let mut bad = Vec::new();
    for (i, cmd) in cmds.iter().enumerate() {
        let base = fresh_dir(&format!("{prefix}{i}"));
        let (d, r) = verdict(&base, cmd);
        if d != "deny" {
            bad.push(format!("[{prefix}{i}] got {d} (reason: {r}) for {cmd:?}"));
        }
    }
    assert!(
        bad.is_empty(),
        "system-harming command must be Deny; not denied:\n{}",
        bad.join("\n")
    );
}

// ---------------------------------------------------------------- targets

/// TARGET: embedded shell string in a python stdin-heredoc body writes a
/// protected gate path.
#[test]
#[ignore = "backlog 0fefcd3b: embedded shell string writing a protected gate path must be Deny"]
fn python_heredoc_os_system_redirect_to_protected_path_is_deny() {
    assert_all_deny(
        "osys",
        &[
            "python3 - <<'PY'\nimport os\nos.system('echo x > .git/hooks/pre-commit')\nPY",
            "python3 - <<'PY'\nimport subprocess\nsubprocess.run('echo x > .git/hooks/pre-commit', shell=True)\nPY",
            "python3 - <<'PY'\nimport os\nos.system('echo x > .claude/settings.json')\nPY",
        ],
    );
}

/// TARGET: embedded shell string that destroys home data.
#[test]
fn python_heredoc_os_system_rm_rf_home_is_deny() {
    assert_all_deny(
        "orm",
        &[
            "python3 - <<'PY'\nimport os\nos.system('rm -rf ~/.ssh')\nPY",
            "python3 - <<'PY'\nimport subprocess\nsubprocess.run('rm -rf ~/.ssh', shell=True)\nPY",
        ],
    );
}

/// TARGET: osascript telling Finder to delete a home/system path (and the
/// exact probe string: empty the trash, also after a python heredoc).
#[test]
#[ignore = "backlog 0fefcd3b: osascript Finder delete must be Deny"]
fn osascript_finder_delete_is_deny() {
    assert_all_deny(
        "osa",
        &[
            "osascript -e 'tell application \"Finder\" to delete POSIX file \"/Users\"'",
            "osascript -e 'tell application \"Finder\" to delete (path to home folder)'",
            "osascript -e 'tell application \"Finder\" to delete POSIX file \"/System/Library\"'",
            "osascript -e 'tell app \"Finder\" to delete every item of trash'",
            "python3 - <<'PY'\nprint(1)\nPY\nosascript -e 'tell app \"Finder\" to delete every item of trash'",
        ],
    );
}

// --------------------------------------------------------------- controls

/// CONTROL: harmless embedded shell string is not Deny.
#[test]
fn control_python_heredoc_os_system_echo_is_not_deny() {
    let cmd = "python3 - <<'PY'\nimport os\nos.system('echo hi')\nPY";
    let base = fresh_dir("ctl-echo");
    let (d, r) = verdict(&base, cmd);
    assert_ne!(
        d, "deny",
        "harmless os.system must not be Deny; reason: {r}"
    );
}

/// CONTROL: harmless osascript is not Deny.
#[test]
fn control_osascript_display_notification_is_not_deny() {
    let cmd = "osascript -e 'display notification \"x\"'";
    let base = fresh_dir("ctl-osa");
    let (d, r) = verdict(&base, cmd);
    assert_ne!(
        d, "deny",
        "harmless osascript must not be Deny; reason: {r}"
    );
}

/// CONTROL: the same redirect at top level stays Deny.
#[test]
fn control_top_level_redirect_to_protected_path_stays_deny() {
    assert_all_deny("top", &["echo x > .git/hooks/pre-commit"]);
}
