#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent verification probes for backlog 8f8acf5e (sudo/doas shell mode
//! with a command => max(Ask floor, direct argv, joined words as shell body)).
//! UNTRACKED verifier file.

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
        "bg-verify8f8-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, subagent: bool) -> (String, String) {
    let home = unique_home("t");
    let mut p = serde_json::json!({
        "session_id": "verify-8f8acf5e-20261127",
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

/// Collect every mismatch so one run reports all of them.
fn check(cases: &[(&str, bool, &str)]) {
    let mut bad = Vec::new();
    for (c, sub, want) in cases {
        let (d, r) = run_as(c, *sub);
        let ok = match *want {
            "not-allow" => d != "allow",
            "not-deny" => d != "deny",
            w => d == w,
        };
        if !ok {
            bad.push(format!("{c:?} (subagent={sub}) want {want}, got {d} | {r}"));
        }
    }
    assert!(bad.is_empty(), "mismatches:\n{}", bad.join("\n"));
}

fn deny_both(cs: &[&str]) {
    let mut v = Vec::new();
    for c in cs {
        v.push((*c, false, "deny"));
        v.push((*c, true, "deny"));
    }
    check(&v);
}

#[test]
fn option_forms_reach_the_body() {
    deny_both(&[
        "sudo -u root -s 'rm -rf /etc'",
        "sudo -uroot -s 'rm -rf /etc'",
        "sudo -s -u root 'rm -rf /etc'",
        "sudo -s -- 'rm -rf /etc'",
        "sudo -E -s 'rm -rf /etc'",
        "sudo -i FOO=bar 'rm -rf /etc'",
        "sudo --shell --user=root 'rm -rf /etc'",
        "sudo --login --preserve-env 'rm -rf /etc'",
        "doas -u root -s 'rm -rf /etc'",
    ]);
}

#[test]
#[ignore = "RED(1e915597): main mode `sudo -sE 'rm -rf /etc'` / `sudo -Es 'rm -rf /etc'` are Ask (`sudo` option `-sE` is not modelled), not Deny; unquoted `-Es rm -rf /etc` is Deny"]
fn clustered_short_options() {
    deny_both(&[
        "sudo -sE 'rm -rf /etc'",
        "sudo -Es 'rm -rf /etc'",
        "sudo -Es rm -rf /etc",
        "sudo -nEs rm -rf /etc",
    ]);
}

#[test]
fn multi_word_joins() {
    deny_both(&[
        "sudo -s rm -rf /etc",
        "sudo -s 'rm -rf' '/etc'",
        "sudo -s 'rm' '-rf /etc'",
        "sudo -s 'echo ok; rm -rf /etc'",
        "sudo -s 'cd /; rm -rf etc'",
        "sudo -s 'true &&' 'rm -rf /etc'",
        "sudo -s 'rm -rf $HOME'",
    ]);
}

#[test]
fn escaping_divergence_dollar_home() {
    deny_both(&["sudo -s rm -rf \"'\" '$HOME' \"'\""]);
}

#[test]
fn nesting_and_wrappers() {
    deny_both(&[
        "sudo sudo -s 'rm -rf /etc'",
        "sudo -s sudo -s 'rm -rf /etc'",
        "env sudo -s 'rm -rf /etc'",
        "nohup sudo -s 'rm -rf /etc'",
        "timeout 5 sudo -s 'rm -rf /etc'",
        "bash -c \"sudo -s 'rm -rf /etc'\"",
        "(sudo -s 'rm -rf /etc')",
        "true && sudo -s 'rm -rf /etc'",
        "sudo -s 'rm -rf /etc' | cat",
    ]);
}

#[test]
fn undetermined_words_never_allow() {
    let mut v = Vec::new();
    for c in [
        "sudo -s \"$X\"",
        "sudo -s ls \"$X\"",
        "sudo -s 'rm -rf /etc' \"$X\"",
        "sudo -s \"$(echo rm -rf /etc)\"",
        "sudo -i \"$X\" 'rm -rf /etc'",
    ] {
        v.push((c, false, "not-allow"));
        v.push((c, true, "deny"));
    }
    check(&v);
}

#[test]
fn harmless_bodies_stay_ask_in_main() {
    let mut v = Vec::new();
    for c in [
        "sudo -s 'ls -la'",
        "sudo -i 'ls -la'",
        "sudo --shell ls -la",
        "doas -s ls",
        "sudo -s 'echo hi'",
    ] {
        v.push((c, false, "ask"));
        v.push((c, true, "deny"));
    }
    check(&v);
}

#[test]
fn sudoedit_not_regressed() {
    check(&[
        ("sudo -e /etc/hosts", false, "ask"),
        ("sudo --edit /etc/hosts", false, "ask"),
        ("sudo -l", false, "not-deny"),
    ]);
}
