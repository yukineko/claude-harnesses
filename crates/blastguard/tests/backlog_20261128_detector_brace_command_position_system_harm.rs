#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: system harm spelled with a brace expansion in command position is Ask.
//!
//! bash expands `{rm,-rf} /etc` to `rm -rf /etc` (the test proves it with
//! `bash -c 'echo ...'`), which is Deny when spelled out. Ruling (memory
//! `system-harm-always-deny`): harmful targets are Deny through any wrapper
//! or spelling. Measured 2026-11-28 on main 6cc6d735d: `{rm,-rf} /etc` and
//! `{rm,-rf,/etc}` are Ask. An Ask can be approved by a human in a hurry; a
//! Deny cannot.
//!
//! Controls (GREEN): `rm -rf /etc` and the already-handled argument-position
//! braces (`rm {-rf,x} /etc`, `rm -rf {/etc,x}`) are Deny; a harmless
//! command-position brace (`{echo,hi}`) is not Deny.
//!
//! DOES NOT PROVE: other system targets, other harm kinds, subagent mode.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::PathBuf;
use std::process::{Command, Stdio};

struct Fx {
    _dir: tempfile::TempDir,
    base: PathBuf,
}

impl Fx {
    fn new() -> Fx {
        std::fs::create_dir_all("/private/tmp/claude-askrules").unwrap();
        let dir = tempfile::Builder::new()
            .prefix("detector-brace-cmdpos-")
            .tempdir_in("/private/tmp/claude-askrules")
            .unwrap();
        let base = dir.path().to_path_buf();
        std::fs::create_dir_all(base.join("home")).unwrap();
        std::fs::create_dir_all(base.join("work")).unwrap();
        Fx { _dir: dir, base }
    }
}

fn bash_expand(cmd: &str) -> String {
    let out = Command::new("bash")
        .arg("-c")
        .arg(format!("echo {cmd}"))
        .output()
        .unwrap();
    assert!(out.status.success());
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn verdict(fx: &Fx, cmd: &str) -> String {
    let cwd = fx.base.join("work");
    let payload = serde_json::json!({
        "session_id": "detector-brace-cmdpos",
        "hook_event_name": "PreToolUse",
        "cwd": cwd,
        "tool_name": "Bash",
        "tool_input": { "command": cmd }
    })
    .to_string();
    let mut hook = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    git_env::scrub(&mut hook)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .env("HOME", fx.base.join("home"))
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(&fx.base);
    let out = bounded::run_bounded(&mut hook, payload.as_bytes(), bounded::HOOK_DEADLINE);
    assert_eq!(out.status.code(), Some(0), "hook must always exit 0");
    let s = String::from_utf8_lossy(&out.stdout).into_owned();
    if s.trim().is_empty() {
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(s.trim()).expect("hook JSON");
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or_else(|| panic!("no permissionDecision: {s}"))
        .to_string()
}

const BRACED: [&str; 2] = ["{rm,-rf} /etc", "{rm,-rf,/etc}"];

#[test]
fn control_braced_forms_expand_to_rm_rf_etc() {
    for c in BRACED {
        assert_eq!(bash_expand(c), "rm -rf /etc", "fixture: {c}");
    }
}

#[test]
fn control_spelled_and_argument_braces_are_deny() {
    let fx = Fx::new();
    for c in ["rm -rf /etc", "rm {-rf,x} /etc", "rm -rf {/etc,x}"] {
        assert_eq!(verdict(&fx, c), "deny", "{c}");
    }
}

#[test]
fn control_harmless_command_position_brace_is_not_deny() {
    let fx = Fx::new();
    assert_ne!(verdict(&fx, "{echo,hi}"), "deny");
}

#[test]
#[ignore = "RED(46145358): pending"]
fn command_position_brace_system_harm_is_deny() {
    let fx = Fx::new();
    let bad: Vec<String> = BRACED
        .iter()
        .map(|c| (verdict(&fx, c), *c))
        .filter(|(v, _)| v != "deny")
        .map(|(v, c)| format!("{c} => {v}"))
        .collect();
    assert!(
        bad.is_empty(),
        "braced rm -rf /etc is not Deny:\n{}",
        bad.join("\n")
    );
}
