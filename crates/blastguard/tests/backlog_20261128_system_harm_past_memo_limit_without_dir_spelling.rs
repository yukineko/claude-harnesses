#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog RED (2026-11-28, independent verifier of 687ece3c fix 1e0bef688):
//! past the shallow memo limit (600 distinct nested texts) only texts that
//! literally spell a SYSTEM_PREFIXES dir are still judged; a statically
//! resolvable system-harm tail that does NOT spell one (`cd / && rm -rf etc`,
//! `D=etc; rm -rf /$D`) comes back Ask in main mode (subagent Deny). Without
//! the padding the same tails are Deny. Repo rule: system harm always Deny.
//!
//! Contract: Deny in main and subagent mode.
#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

struct Home(std::path::PathBuf);
impl Drop for Home {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn run_as(command: &str, subagent: bool) -> (String, Duration) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let home = Home(std::env::temp_dir().join(format!(
        "bg-nested-harm-padding-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )));
    std::fs::create_dir_all(home.0.join("wt")).unwrap();
    let mut p = serde_json::json!({
        "session_id": "backlog-20261128-nested-harm-padding",
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
    let t0 = Instant::now();
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), Duration::from_secs(60));
    let took = t0.elapsed();
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".into(), took);
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    (
        v["hookSpecificOutput"]["permissionDecision"]
            .as_str()
            .unwrap_or("?")
            .to_string(),
        took,
    )
}


fn check(label: &str, command: &str, bad: &mut Vec<String>) {
    for sub in [false, true] {
        let (d, took) = run_as(command, sub);
        println!("{label} sub={sub} -> {d} in {}ms", took.as_millis());
        if d != "deny" {
            bad.push(format!("{label} sub={sub} -> {d} in {}ms", took.as_millis()));
        }
    }
}

fn tails() -> Vec<(&'static str, String)> {
    let cd = concat!("cd / && rm -", "rf etc");
    let var = concat!("D=e", "tc; rm -", "rf /$D");
    vec![
        ("bash-c cdroot", format!("bash -c '{cd}'")),
        ("eval cdroot", format!("eval '{cd}'")),
        ("bash-c varsplit", format!("bash -c '{var}'")),
        ("eval varsplit", format!("eval '{var}'")),
    ]
}

#[test]
fn control_tails_without_padding_are_deny() {
    let mut bad = Vec::new();
    for (name, t) in tails() {
        check(&format!("nopad {name}"), &t, &mut bad);
    }
    assert!(bad.is_empty(), "control not denied:\n{}", bad.join("\n"));
}

#[test]
#[ignore = "RED(5eab530d): statically resolvable system harm past the memo limit stays Ask in main when its text does not spell a system dir"]
fn harm_not_spelling_a_system_dir_after_memo_limit_is_deny() {
    let mut bad = Vec::new();
    let lines: String = (0..3700).map(|i| format!("bash -c 'echo {i}'\n")).collect();
    for (name, t) in tails() {
        check(&format!("fill3700 + {name}"), &format!("{lines}{t}"), &mut bad);
    }
    assert!(bad.is_empty(), "not denied:\n{}", bad.join("\n"));
}

/// The D4 fan-out shape from `src/detect/budget.rs`.
fn fanout(n: usize) -> String {
    let mut s = String::from("find . ");
    for _ in 0..n {
        s.push_str("-exec find . ");
    }
    s.push_str("-exec echo {} ");
    for _ in 0..=n {
        s.push_str("+ ");
    }
    s
}

/// Regression vs base aed5625a6 (blastguard 0.2.974): these were Deny in main
/// there and are Ask on 1e0bef688 (0.2.975) — the shallow memo is now filled
/// by the python fan-out padding, so the tail is past the memo limit.
#[test]
fn python_padding_then_unspelled_system_harm_is_deny() {
    let mut bad = Vec::new();
    let pad = format!("python3 -c \"import os; os.system('{}')\"\n", fanout(32)).repeat(2);
    let rmroot = concat!("rm -", "rf /");
    let cd = concat!("cd / && rm -", "rf etc");
    for (name, t) in [
        ("bash-c rmroot", format!("bash -c '{rmroot}'")),
        ("eval rmroot", format!("eval '{rmroot}'")),
        ("cmdsub rmroot", format!("echo $({rmroot})")),
        ("bash-c cdroot", format!("bash -c '{cd}'")),
        ("eval cdroot", format!("eval '{cd}'")),
    ] {
        check(&format!("py2 + {name}"), &format!("{pad}{t}"), &mut bad);
    }
    assert!(bad.is_empty(), "not denied:\n{}", bad.join("\n"));
}
