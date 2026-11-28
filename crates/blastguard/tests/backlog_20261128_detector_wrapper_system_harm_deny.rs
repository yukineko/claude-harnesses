#![allow(clippy::expect_used, clippy::unwrap_used, clippy::panic)]
//! RED: system harm behind `xcrun`, `watch`, `caffeinate`, `arch`, `chronic`,
//! `script -q /dev/null` or `unbuffer` is Ask, not the bare form's Deny.
//!
//! Ruling (memory `system-harm-always-deny`): harmful targets are Deny
//! through any wrapper. `rm -rf /etc` is Deny bare; each wrapper runs its
//! argv as a child command, so the wrapped form deletes the same tree.
//! Measured 2026-11-28 on main b2203bbe7 (blastguard 0.2.990): every wrapper
//! below gives Ask in an interactive session. An Ask can be approved by a
//! human in a hurry; a Deny cannot.
//!
//! Controls (GREEN): bare `rm -rf /etc` is Deny; the wrappers already
//! handled as wrappers (`env`, `command`, `nice`, `timeout 5`) give Deny;
//! a wrapped harmless `ls` is Allow (no over-block).
//!
//! Harness: payload `cwd` and hook `HOME` are a fixture under
//! `/private/tmp/claude-askrules/` (not `/var/folders`, which blastguard
//! treats as a system directory).
//!
//! DOES NOT PROVE: subagent mode (Ask hardens to Deny there anyway); system
//! targets other than `/etc`; harm kinds other than recursive delete.
#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

const WRAPPERS: [&str; 7] = [
    "xcrun",
    "watch",
    "caffeinate",
    "arch",
    "chronic",
    "script -q /dev/null",
    "unbuffer",
];

fn base() -> PathBuf {
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let b = PathBuf::from(format!(
        "/private/tmp/claude-askrules/detector-wrapper-harm-{}-{nanos}-{}",
        std::process::id(),
        // libtest names each test thread after its test: unique per test.
        std::thread::current().name().unwrap_or("main")
    ));
    std::fs::create_dir_all(b.join("home")).unwrap();
    std::fs::create_dir_all(b.join("work")).unwrap();
    b
}

fn verdict(b: &Path, cmd: &str) -> String {
    let cwd = b.join("work");
    let payload = serde_json::json!({
        "session_id": "detector-wrapper-harm",
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
        .env("HOME", b.join("home"))
        .env("CLAUDE_PROJECT_DIR", &cwd)
        .env("BLASTGUARD_APPROVALS_DIR", b.join("store"))
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("BLASTGUARD_ASK")
        .env_remove("TMPDIR")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .current_dir(b);
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

#[test]
fn control_bare_and_known_wrapper_system_harm_is_deny() {
    let b = base();
    for c in [
        "rm -rf /etc",
        "env rm -rf /etc",
        "command rm -rf /etc",
        "nice rm -rf /etc",
        "timeout 5 rm -rf /etc",
    ] {
        assert_eq!(verdict(&b, c), "deny", "{c}");
    }
}

#[test]
fn control_wrapped_harmless_is_allow() {
    let b = base();
    for w in WRAPPERS {
        let c = format!("{w} ls");
        assert_eq!(verdict(&b, &c), "allow", "over-block: {c}");
    }
}

#[test]
#[ignore = "RED pending-id"]
fn wrapped_system_harm_is_deny() {
    let b = base();
    let bad: Vec<String> = WRAPPERS
        .iter()
        .filter_map(|w| {
            let c = format!("{w} rm -rf /etc");
            let v = verdict(&b, &c);
            (v != "deny").then(|| format!("{v} (bare is deny): {c}"))
        })
        .collect();
    assert!(
        bad.is_empty(),
        "{} wrapped system harms weaker than bare:\n{}",
        bad.len(),
        bad.join("\n")
    );
}
