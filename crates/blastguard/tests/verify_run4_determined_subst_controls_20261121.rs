#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Controls for backlog 11b0b4da / 28019c35 / d766b5fe (run-20261121-114321-94421),
//! written by the orchestrator, not by the implementers.
//!
//! (1) benign twins of the three RED shapes must not become Deny (the fix must
//!     not make a benign determined program more restrictive than Ask);
//! (2) shapes whose program is NOT determined, or whose names are rebound on
//!     the line, must not become Allow (fail-closed preserved);
//! (3) close siblings of the harmful shapes must be Deny (the fix is not
//!     keyed to the exact spelling of the RED).
//!
//! Groups (2) and (3) run in main and subagent mode; group (1) runs in main
//! mode only (a subagent's Ask is delivered as Deny). Decisions only.
//!
//! DOES NOT PROVE: reason text; Allow for the benign twins (only "not Deny");
//! real shell behaviour (nothing is executed).

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
        "bg-run4-ctl-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "run4-determined-subst-controls-20261121",
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

fn check(cmd: &str, tag: &str, ok: fn(&str) -> bool, want: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let d = decide(cmd, &home, subagent);
        if !ok(&d) {
            bad.push(format!("subagent={subagent} => {d}"));
        }
    }
    assert!(
        bad.is_empty(),
        "expected {want} for {cmd:?}:\n{}",
        bad.join("\n")
    );
}

/// Main mode only: in subagent mode every Ask is delivered as Deny (nobody can
/// answer the prompt), so "not Deny" is only a property of the main-mode verdict.
fn not_deny(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let d = decide(cmd, &home, false);
    assert!(
        d == "allow" || d == "ask",
        "expected not Deny (main mode) for {cmd:?}: {d}"
    );
}

fn not_allow(cmd: &str, tag: &str) {
    check(cmd, tag, |d| d == "ask" || d == "deny", "not Allow");
}

fn deny(cmd: &str, tag: &str) {
    check(cmd, tag, |d| d == "deny", "Deny");
}

// ------------------------------------------------ (1) benign twins: not Deny

#[test]
fn benign_echo_e_newline_split_in_procsub_is_not_deny() {
    not_deny(r#"bash <(echo -e 'echo hi\necho there')"#, "b1");
}

#[test]
fn benign_bash_c_of_determined_subst_is_not_deny() {
    not_deny(r#"bash -c "$(echo ls)""#, "b2");
}

#[test]
fn benign_eval_of_determined_subst_through_cat_is_not_deny() {
    not_deny(r#"eval "$(echo ls | cat)""#, "b3");
}

// ------------------------------------- (2) undetermined / rebound: not Allow

#[test]
fn bash_c_of_file_reading_subst_is_not_allow() {
    not_allow(r#"bash -c "$(cat ./prog.sh)""#, "u1");
}

#[test]
fn eval_of_file_reading_subst_is_not_allow() {
    not_allow(r#"eval "$(cat ./prog.sh)""#, "u2");
}

#[test]
fn echo_shadowed_then_bash_c_subst_is_not_allow() {
    not_allow(
        r#"echo(){ printf 'chmod 777 /etc/hosts'; }; bash -c "$(echo ls)""#,
        "u3",
    );
}

#[test]
fn cat_shadowed_then_eval_subst_is_not_allow() {
    not_allow(
        r#"cat(){ printf 'chmod 777 /etc/hosts'; }; eval "$(echo ls | cat)""#,
        "u4",
    );
}

#[test]
fn echo_e_tab_escape_in_procsub_is_not_allow() {
    not_allow(r#"bash <(echo -e 'echo hi\trm -rf /etc')"#, "u5");
}

#[test]
fn echo_without_e_newline_in_procsub_is_not_allow() {
    not_allow(r#"bash <(echo 'echo hi\nrm -rf /etc')"#, "u6");
}

// ------------------------------------------- (3) harmful siblings: Deny

#[test]
fn echo_e_newline_harm_first_line_in_procsub_is_deny() {
    deny(r#"bash <(echo -e 'rm -rf /etc\necho hi')"#, "d1");
}

#[test]
fn sh_c_of_determined_harmful_subst_is_deny() {
    deny(r#"sh -c "$(echo 'rm -rf /etc')""#, "d2");
}

#[test]
fn eval_of_determined_harmful_subst_is_deny() {
    deny(r#"eval "$(echo 'rm -rf /etc')""#, "d3");
}
