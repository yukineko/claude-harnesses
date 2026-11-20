#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Round 2 adversarial tests for backlog e1bb5f7e: literal here-string /
//! heredoc stdin behind sudo/doas, and sudo -l/-v. Independent test author.
//! Every probe runs in main and subagent mode; empty stdout = allow.

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
        "bg-e1bb-r2-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "e1bb5f7e-adversarial-r2-20261120",
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

fn check(tag: &str, cmd: &str, ok: impl Fn(&str) -> bool, what: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for sub in [false, true] {
        let (d, r) = decide(cmd, &home, sub);
        eprintln!("OBSERVED {cmd:?} subagent={sub} => {d}");
        if !ok(&d) {
            bad.push(format!("subagent={sub} => {d} ({r})"));
        }
    }
    assert!(bad.is_empty(), "{what}: {cmd:?}: {bad:?}");
}

fn is_deny(tag: &str, cmd: &str) {
    check(tag, cmd, |d| d == "deny", "system harm must be Deny");
}
fn not_allow(tag: &str, cmd: &str) {
    check(tag, cmd, |d| d != "allow", "fail-open, must be ask/deny");
}
fn not_deny(tag: &str, cmd: &str) {
    check(
        tag,
        cmd,
        |d| d != "deny",
        "false positive, must not be Deny",
    );
}

#[test]
fn sudo_bash_herestring_harm() {
    is_deny("a1", "sudo bash <<< 'rm -rf /etc'");
}

#[test]
fn sudo_s_herestring_harm() {
    is_deny("a2", "sudo -s <<< 'rm -rf /etc'");
}

#[test]
fn doas_s_herestring_harm() {
    is_deny("a3", "doas -s <<< 'rm -rf /etc'");
}

#[test]
fn doas_bash_herestring_harm() {
    is_deny("a4", "doas bash <<< 'rm -rf /etc'");
}

#[test]
fn sudo_u_root_sh_herestring_usr() {
    is_deny("a5", "sudo -u root sh <<< 'rm -rf /usr'");
}

#[test]
fn sudo_s_heredoc_quoted_harm() {
    is_deny("b1", "sudo -s <<'E'\nrm -rf /etc\nE");
}

#[test]
fn sudo_bash_heredoc_quoted_harm() {
    is_deny("b2", "sudo bash <<'E'\nrm -rf /etc\nE");
}

#[test]
fn sudo_bash_heredoc_unquoted_harm() {
    is_deny("b3", "sudo bash <<E\nrm -rf /etc\nE");
}

#[test]
fn sudo_python3_herestring_harm_not_allow() {
    not_allow(
        "c1",
        "sudo python3 <<< 'import shutil; shutil.rmtree(\"/etc\")'",
    );
}

#[test]
fn sudo_l_with_command_not_allow() {
    not_allow("c2", "sudo -l rm");
}

#[test]
fn sudo_l_u_bob_not_deny() {
    not_deny("c3", "sudo -l -U bob");
}

#[test]
#[ignore = "backlog RED: sudo perl <<< 'print 1' (benign) is ask in main but deny in subagent (perl stdin Undetermined)"]
fn sudo_perl_herestring_not_deny() {
    not_deny("c4", "sudo perl <<< 'print 1'");
}

#[test]
fn sudo_i_herestring_benign_not_deny() {
    not_deny("c5", "sudo -i <<< 'echo hi'");
}

#[test]
fn sudo_bash_herestring_ls_not_deny() {
    not_deny("c6", "sudo bash <<< 'ls'");
}

#[test]
fn ctl_bash_herestring_harm_deny() {
    is_deny("d1", "bash <<< 'rm -rf /etc'");
}

#[test]
fn sudo_env_bash_herestring_harm() {
    is_deny("e1", "sudo env bash <<< 'rm -rf /etc'");
}

#[test]
fn sudo_nice_bash_herestring_harm() {
    is_deny("e2", "sudo nice -n 5 bash <<< 'rm -rf /etc'");
}

#[test]
fn sudo_dashdash_bash_herestring_harm() {
    is_deny("e3", "sudo -- bash <<< 'rm -rf /etc'");
}

#[test]
fn sudo_i_herestring_cmdsubst_not_allow() {
    not_allow("e4", "sudo -i <<< \"$(echo rm -rf /etc)\"");
}

#[test]
fn sudo_i_herestring_home_harm() {
    is_deny("e5", "sudo -i <<< 'rm -rf ~'");
}

#[test]
fn sudo_s_explicit_fd0_herestring_harm() {
    is_deny("e6", "sudo -s 0<<< 'rm -rf /etc'");
}
