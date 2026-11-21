#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Fail-open reported by a verifier (2026-11-22, not re-executed by the
//! reporter): a write `echo x > d/hosts` is Allow although an earlier segment
//! of the SAME line creates `d` as a symlink to a system directory, when that
//! `ln -s` is carried by a shape other than a plain top-level command:
//!   - a vim `-c '!...'` shell escape,
//!   - a `$(...)` command substitution inside an argument (echo / jq),
//!   - a backtick substitution,
//!   - a `bash -c '...'` body.
//! The write lands under `/etc` (`/etc/bg-sentinel/hosts`), so by user ruling
//! (memory `system-harm-always-deny`: harmful targets are Deny through any
//! wrapper) the only acceptable verdict is exactly Deny — not Ask, not Allow.
//!
//! `/etc/bg-sentinel` is a deliberately non-existent sentinel path; nothing in
//! this file runs the commands, they are only fed to the hook as JSON.
//!
//! Fixture: hermetic HOME, cwd = HOME/wt (an ordinary empty project-like
//! directory, same as `backlog_20261121_relink_unmodelled_ask_masks_system_write.rs`),
//! main session (CLAUDECODE=1, entrypoint cli). Empty hook stdout = allow.
//!
//! Controls: `echo x > /etc/hosts` is Deny (spelled system target), and
//! `echo x > d/hosts` with no `ln` anywhere is NOT Deny (so a Deny on the
//! relink shapes is caused by the relink, not by the relative target alone).
//!
//! Written by an independent test author (not the implementer).
//!
//! DOES NOT PROVE: subagent mode; hard links / `mv` relinks; other writers
//! (tee, cp, dd); that the reason text names the relink; that every other
//! substitution/wrapper shape is covered.

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
        "bg-relink-subst-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_main(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "relink-symlink-via-subst-failopen-20261122",
        "cwd": home.0.join("wt"),
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

fn assert_deny(tag: &str, c: &str) {
    let home = unique_home(tag);
    let (d, r) = run_main(c, &home);
    assert_eq!(
        d, "deny",
        "system write through same-line symlink must be exactly deny: {c:?} => {d} ({r})"
    );
}

#[test]
fn vim_shell_escape_symlink_then_write_is_deny() {
    assert_deny(
        "vim",
        "vim -c 'silent !ln -s /etc/bg-sentinel d' -c q; echo x > d/hosts",
    );
}

#[test]
fn echo_dollar_paren_symlink_then_write_is_deny() {
    assert_deny(
        "echo-subst",
        "echo \"$(ln -s /etc/bg-sentinel d)\"; echo x > d/hosts",
    );
}

#[test]
fn jq_arg_dollar_paren_symlink_then_write_is_deny() {
    assert_deny(
        "jq-subst",
        "jq . \"$(ln -s /etc/bg-sentinel d)\"; echo x > d/hosts",
    );
}

#[test]
fn echo_backtick_symlink_then_write_is_deny() {
    assert_deny(
        "backtick",
        "echo `ln -s /etc/bg-sentinel d`; echo x > d/hosts",
    );
}

#[test]
fn bash_c_symlink_then_write_is_deny() {
    assert_deny(
        "bash-c",
        "bash -c 'ln -s /etc/bg-sentinel d'; echo x > d/hosts",
    );
}

#[test]
fn unquoted_dollar_paren_symlink_and_write_joined_by_and_is_deny() {
    // Same property, different joiner and an unquoted substitution.
    assert_deny(
        "and-subst",
        "true $(ln -s /etc/bg-sentinel d) && echo x > d/hosts",
    );
}

#[test]
fn control_spelled_system_write_is_deny() {
    let home = unique_home("ctl-sys");
    let c = "echo x > /etc/hosts";
    let (d, r) = run_main(c, &home);
    assert_eq!(d, "deny", "{c:?} => {d} ({r})");
}

#[test]
fn control_relative_write_without_relink_is_not_deny() {
    let home = unique_home("ctl-rel");
    let c = "echo x > d/hosts";
    let (d, r) = run_main(c, &home);
    assert_ne!(d, "deny", "{c:?} => {d} ({r})");
}
