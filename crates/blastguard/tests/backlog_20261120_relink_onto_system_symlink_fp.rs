#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! CLAIM under test (reviewer, unverified): commit 20b856a14 made cp/mv/install/ln
//! destination checks also resolve the destination physically
//! (system_landing_block -> real_system_path_block -> physical_landing, which
//! follows a FINAL symlink component). `ln -sf`, `mv` and `install` REPLACE an
//! existing symlink at the destination instead of writing through it, so a project
//! path `venv/bin/python` that is already a symlink to /opt/homebrew/bin/python3
//! should not be Deny; the claim is that it now is (a new false positive).
//! Fixture: hermetic HOME, HOME/proj/venv/bin/python -> /opt/homebrew/bin/python3
//! (dangling is fine), cwd = HOME/proj. Empty hook stdout = allow.
//! Control: `cp a conf` where HOME/proj/conf -> /etc/hosts writes THROUGH the
//! link and must be deny.
//!
//! DOES NOT PROVE: behaviour for other writers (tee, dd of=, redirects), for
//! non-final symlink components, for a symlink whose target is a real existing
//! system file, for `ln` without -f, or that a deny here is wrong for any reason
//! other than the replace-vs-write-through distinction.
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
        "bg-relink-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

fn run_in(command: &str, home: &Home, cwd: &std::path::Path) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "relink-20261120",
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

/// HOME/proj/venv/bin/python -> /opt/homebrew/bin/python3 ; returns (home, cwd).
fn venv_fixture(tag: &str) -> (Home, std::path::PathBuf) {
    let home = unique_home(tag);
    let proj = home.0.join("proj");
    std::fs::create_dir_all(proj.join("venv").join("bin")).unwrap();
    std::os::unix::fs::symlink(
        "/opt/homebrew/bin/python3",
        proj.join("venv").join("bin").join("python"),
    )
    .unwrap();
    (home, proj)
}

fn check_not_denied(tag: &str, command: &str) {
    let (home, cwd) = venv_fixture(tag);
    let (m, r) = run_in(command, &home, &cwd);
    assert_ne!(
        m, "deny",
        "replaces the symlink at the destination, does not write through it; command={command} decision={m} reason={r}"
    );
}

#[test]
fn ln_sf_onto_existing_system_symlink_is_not_denied() {
    check_not_denied("ln", "ln -sf /opt/homebrew/bin/python3 venv/bin/python");
}

#[test]
fn mv_onto_existing_system_symlink_is_not_denied() {
    check_not_denied("mv", "mv newlink venv/bin/python");
}

#[test]
fn install_onto_existing_system_symlink_is_not_denied() {
    check_not_denied("install", "install -m755 x venv/bin/python");
}

#[test]
fn cp_through_existing_system_symlink_into_etc_is_denied() {
    let home = unique_home("cp-etc");
    let proj = home.0.join("proj");
    std::fs::create_dir_all(&proj).unwrap();
    std::os::unix::fs::symlink("/etc/hosts", proj.join("conf")).unwrap();
    let cmd = "cp a conf";
    let (m, r) = run_in(cmd, &home, &proj);
    assert_eq!(
        m, "deny",
        "cp writes THROUGH the link to /etc/hosts; command={cmd} decision={m} reason={r}"
    );
}
