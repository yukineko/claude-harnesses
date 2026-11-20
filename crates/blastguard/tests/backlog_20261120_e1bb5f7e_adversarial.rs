#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Adversarial tests for the sudo/doas arm of b5 `unwrap_at` (backlog e1bb5f7e,
//! spec bg-sudo-stdin-shell-e1bb5f7e R1-R4). Written by an independent test
//! author who did not write the fix. Every probe is run in main mode AND
//! subagent mode (`agent_id` in the payload); empty stdout = allow.

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
        "bg-e1bb-adv-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "e1bb5f7e-adversarial-20261120",
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

/// Property: `cmd` is never Allow (ask or deny) in main and subagent mode.
fn not_allow(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for sub in [false, true] {
        let (d, r) = decide(cmd, &home, sub);
        eprintln!("OBSERVED {cmd:?} subagent={sub} => {d}");
        if d == "allow" {
            bad.push(format!("subagent={sub} => {d} ({r})"));
        }
    }
    assert!(
        bad.is_empty(),
        "fail-open: {cmd:?} must be ask/deny but: {bad:?}"
    );
}

/// Property: system harm stays Deny through the wrapper, main and subagent.
fn is_deny(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for sub in [false, true] {
        let (d, r) = decide(cmd, &home, sub);
        eprintln!("OBSERVED {cmd:?} subagent={sub} => {d}");
        if d != "deny" {
            bad.push(format!("subagent={sub} => {d} ({r})"));
        }
    }
    assert!(bad.is_empty(), "system harm must be Deny: {cmd:?}: {bad:?}");
}

/// Property: no new false positive; the command must not be Deny.
fn not_deny(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for sub in [false, true] {
        let (d, r) = decide(cmd, &home, sub);
        eprintln!("OBSERVED {cmd:?} subagent={sub} => {d}");
        if d == "deny" {
            bad.push(format!("subagent={sub} => {d} ({r})"));
        }
    }
    assert!(bad.is_empty(), "false positive: {cmd:?} denied: {bad:?}");
}

// ---- spec acceptance not yet covered ----

#[test]
fn r2_sudo_i_stdin_not_allow() {
    not_allow("a1", "sudo -i < /tmp/p");
}

#[test]
fn r2_sudo_e_not_allow() {
    not_allow("a2", "sudo -e /tmp/x");
}

#[test]
fn r1_unmodelled_long_option_not_allow() {
    not_allow("a3", "sudo --zz-unmodelled ls");
}

// ---- fail-open probes ----

#[test]
fn attached_user_arg_uroot() {
    not_allow("f1", "sudo -uroot bash < /tmp/p");
}

#[test]
fn double_dash_before_shell() {
    not_allow("f2", "sudo -- bash < /tmp/p");
}

#[test]
fn flags_e_h_before_shell() {
    not_allow("f3", "sudo -E -H bash < /tmp/p");
}

#[test]
fn many_options_n_u_g_before_sh() {
    not_allow("f4", "sudo -n -u root -g wheel sh < /tmp/p");
}

#[test]
fn env_assignment_before_shell() {
    not_allow("f5", "sudo FOO=1 bash < /tmp/p");
}

#[test]
fn sudo_env_bash() {
    not_allow("f6", "sudo env bash < /tmp/p");
}

#[test]
fn sudo_nice_bash() {
    not_allow("f7", "sudo nice bash < /tmp/p");
}

#[test]
fn doas_u_root_bash() {
    not_allow("f8", "doas -u root bash < /tmp/p");
}

#[test]
fn doas_s_stdin() {
    not_allow("f9", "doas -s < /tmp/p");
}

#[test]
fn sudo_sudo_bash() {
    not_allow("f10", "sudo sudo bash < /tmp/p");
}

#[test]
#[ignore = "backlog RED: sudo -i <<< 'rm -rf /etc' is Allow in main and subagent (here-string stdin to implicit shell not judged)"]
fn sudo_i_herestring_harm() {
    not_allow("f11", "sudo -i <<< 'rm -rf /etc'");
}

#[test]
#[ignore = "backlog RED: piping a harm string into sudo bash is ask in main (deny only in subagent), not Deny"]
fn pipe_harm_into_sudo_bash_deny() {
    is_deny("f12", "echo rm -rf /etc | sudo bash");
}

#[test]
#[ignore = "backlog RED: piping a harm string into sudo -s is ask in main (deny only in subagent), not Deny"]
fn pipe_harm_into_sudo_s_deny() {
    is_deny("f13", "echo rm -rf /etc | sudo -s");
}

#[test]
fn ctl_sudo_bash_c_rm_etc_deny() {
    is_deny("f14", "sudo bash -c 'rm -rf /etc'");
}

#[test]
fn sudo_u_root_rm_etc_deny() {
    is_deny("f15", "sudo -u root rm -rf /etc");
}

#[test]
fn sudo_s_with_command_rm_etc_deny() {
    is_deny("f16", "sudo -s rm -rf /etc");
}

#[test]
fn bundled_flags_en_not_allow() {
    not_allow("f17", "sudo -En bash < /tmp/p");
}

#[test]
fn option_c_with_arg() {
    not_allow("f18", "sudo -C 3 bash < /tmp/p");
}

#[test]
fn option_d_with_arg() {
    not_allow("f19", "sudo -D /tmp bash < /tmp/p");
}

#[test]
fn option_h_with_arg() {
    not_allow("f20", "sudo -h localhost bash < /tmp/p");
}

#[test]
fn option_p_empty_arg() {
    not_allow("f21", "sudo -p '' bash < /tmp/p");
}

#[test]
fn option_u_undeterminable_word() {
    not_allow("f22", "sudo -u \"$U\" bash < /tmp/p");
}

#[test]
fn sudo_python_stdin() {
    not_allow("f23", "sudo python3 < /tmp/p");
}

#[test]
fn bash_c_sudo_s_outer_redirect() {
    not_allow("f24", "bash -c 'sudo -s' < /tmp/p");
}

#[test]
fn exec_sudo_bash() {
    not_allow("f25", "exec sudo bash < /tmp/p");
}

#[test]
fn command_sudo_bash() {
    not_allow("f26", "command sudo bash < /tmp/p");
}

#[test]
fn timeout_sudo_bash() {
    not_allow("f27", "timeout 5 sudo bash < /tmp/p");
}

// ---- no-new-FP controls (must not be Deny) ----

#[test]
fn ctl_sudo_ls() {
    not_deny("c1", "sudo ls");
}

#[test]
fn ctl_sudo_u_root_ls() {
    not_deny("c2", "sudo -u root ls");
}

#[test]
#[ignore = "backlog RED: sudo -l (unmodelled option) is ask in main and deny in subagent; base treated sudo as non-wrapper (allow) - new FP"]
fn ctl_sudo_l() {
    not_deny("c3", "sudo -l");
}

#[test]
fn ctl_sudo_k() {
    not_deny("c4", "sudo -k");
}

#[test]
#[ignore = "backlog RED: sudo -v (unmodelled option) is ask in main and deny in subagent; base treated sudo as non-wrapper (allow) - new FP"]
fn ctl_sudo_v() {
    not_deny("c5", "sudo -v");
}

#[test]
fn ctl_sudo_h() {
    not_deny("c6", "sudo -h");
}

#[test]
fn ctl_sudo_s_toplevel() {
    not_deny("c7", "sudo -s");
}

#[test]
fn ctl_sudo_n_true() {
    not_deny("c8", "sudo -n true");
}
