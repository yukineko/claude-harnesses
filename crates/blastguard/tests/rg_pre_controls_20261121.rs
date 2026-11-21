#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Controls for 1f037eb5: plain `rg` (no `--pre`) keeps its decision, and
//! `rg --pre rm -rf /etc` stays not-Allow. Base values observed before the fix.
//! Also guards the regression found in the first attempt (c8e0ff67c): treating
//! `--pre-glob` as a value flag for every head let
//! `gzip -- --pre-glob .githooks/pre-commit` skip its protected operand.
//!
//! DOES NOT PROVE: real rg behaviour (nothing is executed).

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
        "bg-rgctl-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "rg-pre-protected-20261121",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
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
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

fn each_mode(cmd: &str, tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    [false, true]
        .iter()
        .map(|&s| format!("subagent={s} => {}", decide(cmd, &home, s)))
        .collect()
}

fn expect_allow(cmd: &str, tag: &str) {
    let got = each_mode(cmd, tag);
    assert_eq!(
        got,
        vec!["subagent=false => allow", "subagent=true => allow"],
        "{cmd:?}"
    );
}

fn expect_not_allow(cmd: &str, tag: &str) {
    let got = each_mode(cmd, tag);
    assert!(
        got.iter().all(|g| !g.ends_with("=> allow")),
        "expected not Allow for {cmd:?}: {got:?}"
    );
}

#[test]
fn plain_rg_protected_operand_stays_allow() {
    expect_allow("rg pat .githooks/pre-commit", "prot");
}

#[test]
fn plain_rg_ordinary_operand_stays_allow() {
    expect_allow("rg -n foo src/", "ord");
}

#[test]
fn rg_pre_rm_rf_etc_stays_not_allow() {
    expect_not_allow("rg --pre rm -rf /etc", "rm");
}

#[test]
fn unknown_head_pre_glob_does_not_hide_protected_operand() {
    expect_not_allow("gzip -- --pre-glob .githooks/pre-commit", "gzip");
}
