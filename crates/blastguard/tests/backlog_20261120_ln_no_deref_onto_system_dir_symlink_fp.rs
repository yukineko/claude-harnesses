#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! CLAIM under test (reviewer, unverified): `ln -sfn NEW current`, `ln -sfh NEW
//! current` (BSD -n / -h: do not follow a target that is a symlink to a directory)
//! and `mv -h x current` REPLACE the link `current` itself, even when `current` is
//! an existing symlink to a directory. entry_landing in
//! src/detect/protected.rs follows a final symlink that points to a directory and
//! judges the directory, so with `current` -> /usr/local/bin these may be Deny: a
//! false positive. Without -n/-h, `ln -sf /tmp/x current` really creates
//! /usr/local/bin/x, so that control must be deny.
//! Fixture: hermetic HOME, HOME/proj/current -> /usr/local/bin, cwd = HOME/proj.
//! Empty hook stdout = allow.
//!
//! Shapes dropped (man on this macOS, BSD): `ln -T` and `mv -T` do not exist
//! (GNU only); BSD ln's no-deref flags are -h and -n, BSD mv's is -h.
//!
//! DOES NOT PROVE: GNU -T behaviour, other writers, non-final symlink
//! components, a link to a non-system directory, or that a deny is wrong for any
//! reason other than replace-vs-descend.
//!
//! Written by an independent test author, not the implementer.

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-lnnd-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

fn run_in(command: &str, home: &Home, cwd: &std::path::Path) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "lnnd-20261120",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), String::new());
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    let h = &v["hookSpecificOutput"];
    (
        h["permissionDecision"].as_str().unwrap_or("?").to_string(),
        h["permissionDecisionReason"]
            .as_str()
            .unwrap_or("")
            .to_string(),
    )
}

/// HOME/proj/current -> /usr/local/bin ; returns (home, cwd).
fn fixture(tag: &str) -> (Home, std::path::PathBuf) {
    let home = unique_home(tag);
    let proj = home.0.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::os::unix::fs::symlink("/usr/local/bin", proj.join("current")).unwrap();
    (home, proj)
}

fn check_not_denied(tag: &str, command: &str) {
    let (home, cwd) = fixture(tag);
    let (m, r) = run_in(command, &home, &cwd);
    assert_ne!(
        m, "deny",
        "replaces the link itself, does not descend; command={command} decision={m} reason={r}"
    );
}

#[test]
fn ln_sfn_replaces_dir_symlink_is_not_denied() {
    check_not_denied("sfn", "ln -sfn /opt/homebrew/bin current");
}

#[test]
fn ln_sfh_replaces_dir_symlink_is_not_denied() {
    check_not_denied("sfh", "ln -sfh /opt/homebrew/bin current");
}

#[test]
fn mv_h_replaces_dir_symlink_is_not_denied() {
    check_not_denied("mvh", "mv -h x current");
}

#[test]
fn ln_sf_into_dir_symlink_lands_in_system_dir_is_denied() {
    let (home, cwd) = fixture("ctl");
    let cmd = "ln -sf /tmp/x current";
    let (m, r) = run_in(cmd, &home, &cwd);
    assert_eq!(
        m, "deny",
        "creates /usr/local/bin/x; command={cmd} decision={m} reason={r}"
    );
}
