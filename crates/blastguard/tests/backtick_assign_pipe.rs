// このファイルは丸ごと integration test なので unwrap/expect を許可する
// (workspace の [workspace.lints.clippy] は production 向けの deny)。
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! `B=`ls -d P | sort`` (a variable assignment from a backtick command
//! substitution containing a pipe) must get the same verdict as the equivalent
//! `B=$(ls -d P | sort)` form and as the plain pipeline `ls -d P | sort`.
//! The two substitution spellings are semantically identical, so a differing
//! verdict has no safety reason.
//!
//! Each case runs with a fresh HOME so the repeat-deny downgrade cannot
//! change a verdict.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

fn verdict(tag: &str, command: &str) -> (String, String) {
    let home =
        std::env::temp_dir().join(format!("blastguard-bt-assign-{}-{tag}", std::process::id()));
    let _ = std::fs::remove_dir_all(&home);
    std::fs::create_dir_all(&home).expect("mkdir home");
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut hook = Command::new(bin);
    hook.env("HOME", &home)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        eprintln!("[{tag}] allow: ");
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    eprintln!("[{tag}] {decision}: {reason}");
    (decision, reason)
}

/// Passing controls: the plain pipeline and the `$( )` spellings are allowed.
#[test]
fn control_and_dollar_paren_forms_are_allowed() {
    for (tag, cmd) in [
        ("control", "ls -d /tmp/x* | sort"),
        ("dollar", "B=$(ls -d /tmp/x* | sort)"),
        ("dollar_echo", "B=$(ls -d /tmp/x* | sort); echo \"$B\""),
        ("echo_dollar", "echo $(ls | sort)"),
    ] {
        let (d, r) = verdict(tag, cmd);
        assert_eq!(d, "allow", "{cmd:?} must be allowed, got {d}: {r}");
    }
}

/// The backtick spelling is the same substitution as `$( )`; its verdict must
/// match the `$( )` form, and must not claim the command word is an expansion
/// (the command words are `ls` and `sort`, both literal).
#[test]
fn backtick_substitution_with_pipe_matches_dollar_paren() {
    let pairs = [
        (
            "bt",
            "B=`ls -d /tmp/x* | sort`",
            "B=$(ls -d /tmp/x* | sort)",
        ),
        (
            "bt_echo",
            "B=`ls -d /tmp/x* | sort`; echo \"$B\"",
            "B=$(ls -d /tmp/x* | sort); echo \"$B\"",
        ),
        ("echo_bt", "echo `ls | sort`", "echo $(ls | sort)"),
    ];
    for (tag, bt, dp) in pairs {
        let (bt_d, bt_r) = verdict(tag, bt);
        let (dp_d, dp_r) = verdict(&format!("{tag}-dp"), dp);
        assert_eq!(
            bt_d, dp_d,
            "{bt:?} -> {bt_d}: {bt_r} but {dp:?} -> {dp_d}: {dp_r}"
        );
    }
}
