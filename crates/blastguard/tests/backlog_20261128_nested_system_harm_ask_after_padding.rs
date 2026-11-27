#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog RED (2026-11-28, verifier on base 24263a0c8): system-harm commands
//! whose harm is only reachable through nesting come back Ask in main mode
//! (subagent Deny) when preceded by heavy fan-out padding. Repo rule: system
//! harm is always Deny, in main too. Padding shapes: 1..4 python -c fan-out
//! programs, `bash -c '<fanout32>'` x1/x2, ~3200 lines of `echo $(echo a)`.
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

const H: &str = concat!("rm -", "rf /", "etc");

fn tails() -> Vec<(&'static str, String)> {
    vec![
        ("bash-c", format!("bash -c '{H}'")),
        ("eval", format!("eval '{H}'")),
        ("bash-c-bash-c", format!("bash -c \"bash -c '{H}'\"")),
        ("cmdsub", format!("echo $({H})")),
    ]
}

fn python_padding(k: usize) -> String {
    format!("python3 -c \"import os; os.system('{}')\"\n", fanout(32)).repeat(k)
}

fn bash_padding(k: usize) -> String {
    format!("bash -c '{}'\n", fanout(32)).repeat(k)
}

fn check(label: &str, command: &str, bad: &mut Vec<String>) {
    for sub in [false, true] {
        let (d, took) = run_as(command, sub);
        println!("{label} sub={sub} -> {d} in {}ms", took.as_millis());
        if d != "deny" {
            bad.push(format!(
                "{label} sub={sub} -> {d} in {}ms",
                took.as_millis()
            ));
        }
    }
}

#[test]
fn control_tails_without_padding_are_deny() {
    let mut bad = Vec::new();
    for (name, t) in tails() {
        check(&format!("nopad {name}"), &t, &mut bad);
    }
    check("nopad procsub", &format!("$({H})"), &mut bad);
    assert!(bad.is_empty(), "control not denied:\n{}", bad.join("\n"));
}

#[test]
#[ignore = "RED(687ece3c): nested system harm is Ask in main after fan-out padding (command node/budget cap); must be Deny"]
fn nested_system_harm_after_padding_is_deny() {
    let mut bad = Vec::new();
    for k in 1..=4usize {
        for (name, t) in tails() {
            // Observed Deny on this tree (2026-11-28): not a failing shape.
            if k == 1 && name == "cmdsub" {
                continue;
            }
            check(
                &format!("python-c x{k} + {name}"),
                &format!("{}{t}", python_padding(k)),
                &mut bad,
            );
        }
    }
    for k in 1..=2usize {
        check(
            &format!("bash-c-fanout x{k} + eval"),
            &format!("{}eval '{H}'", bash_padding(k)),
            &mut bad,
        );
    }
    let lines = "echo $(echo a)\n".repeat(3200);
    for (name, t) in [
        ("bash-c", format!("bash -c '{H}'")),
        ("eval", format!("eval '{H}'")),
        ("cmdsub", format!("$({H})")),
    ] {
        check(
            &format!("echo-lines3200 + {name}"),
            &format!("{lines}{t}"),
            &mut bad,
        );
    }
    assert!(
        bad.is_empty(),
        "nested system harm not denied after padding:\n{}",
        bad.join("\n")
    );
}
