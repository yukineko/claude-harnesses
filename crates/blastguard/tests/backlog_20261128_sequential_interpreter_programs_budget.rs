#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog RED (2026-11-28, verifier of b37bc26c / d7d4af504): the per-command
//! D4 node budget must bound the work of ONE command, however many interpreter
//! programs it holds. d7d4af504 gives every depth-0 interpreter program its own
//! fresh `MAX_ANALYSIS_NODES` and restores the caller's budget afterwards, so
//! k sequential programs cost ~k x the bound. Measured (release, serial):
//! k sequential `python3 -c "import os; os.system('<D4 find -exec fan-out>')"`
//! + `rm -rf /etc`: k=4 deny in 20452ms, k=10 > 40s (killed). The hook timeout
//! is 10s and a timed-out hook does not block the call (fail-open).
//!
//! NOTE: the deployed pre-fix binary 0.2.929 (not the exact base) measured
//! k=4 deny in 11901ms too, so this contract was not met before d7d4af504
//! either; d7d4af504 makes it worse. This test pins the contract, not the delta.
//!
//! Contract: Deny in both modes, each verdict under 8s (run with --release).

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
        "bg-seq-interp-budget-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )));
    std::fs::create_dir_all(home.0.join("wt")).unwrap();
    let mut p = serde_json::json!({
        "session_id": "backlog-20261128-seq-interp-budget",
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

/// The D4 fan-out shape from `src/detect/budget.rs`: `find . ` +
/// `-exec find . `xN + `-exec echo {} ` + `+ `x(N+1).
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

fn check(label: &str, command: &str, bad: &mut Vec<String>) {
    let budget = Duration::from_secs(8);
    for sub in [false, true] {
        let (d, took) = run_as(command, sub);
        println!("{label} sub={sub} -> {d} in {}ms", took.as_millis());
        if d != "deny" || took > budget {
            bad.push(format!(
                "{label} sub={sub} -> {d} in {}ms",
                took.as_millis()
            ));
        }
    }
}

#[test]
fn sequential_python_c_programs_share_one_budget() {
    let prog = format!("python3 -c \"import os; os.system('{}')\"\n", fanout(32));
    let c = format!("{}rm -rf /etc", prog.repeat(4));
    let mut bad = Vec::new();
    check("python-c k=4", &c, &mut bad);
    assert!(bad.is_empty(), "non-deny or over 8s:\n{}", bad.join("\n"));
}

#[test]
fn sequential_python_heredoc_programs_share_one_budget() {
    let prog = format!(
        "python3 - <<'EOF'\nimport os\nos.system('{}')\nEOF\n",
        fanout(32)
    );
    let c = format!("{}rm -rf /etc", prog.repeat(4));
    let mut bad = Vec::new();
    check("python-heredoc k=4", &c, &mut bad);
    assert!(bad.is_empty(), "non-deny or over 8s:\n{}", bad.join("\n"));
}
