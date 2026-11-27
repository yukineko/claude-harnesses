#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! backlog RED (2026-11-28, verifier of b37bc26c / aa0680e6c, blastguard
//! 0.2.960): the per-command nested-visit cap `MAX_COMMAND_NODES = 3000`
//! (src/detect/budget.rs) turns a system-harm Deny into an Ask in main.
//!
//! Observed (release, `tests/zz_probe_command_cap2.rs`, since deleted):
//! a python-stdin heredoc whose program first runs k harmless D4 fan-out
//! `os.system('<find -exec fan-out N=32>')` calls and then
//! `os.system("bash -c '<rm -rf /etc>'")`:
//!   k=2: base 24263a0c8 main=deny sub=deny; tip aa0680e6c main=ask sub=deny
//!   k=3: base 24263a0c8 main=deny sub=deny; tip aa0680e6c main=ask sub=deny
//! Repo ruling (2026-11-21, 1dad1f35): system harm is always Deny, never
//! handed back as a question. The fan-out calls spend the command-wide total
//! before the harmful call is reached; the nested `bash -c` visit then gets
//! the budget Ask.
//!
//! A second shape (procsub tail after k python -c fan-out programs) is below.
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
        "bg-cmd-node-cap-harm-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    )));
    std::fs::create_dir_all(home.0.join("wt")).unwrap();
    let mut p = serde_json::json!({
        "session_id": "backlog-20261128-cmd-node-cap-harm",
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

const H: &str = concat!("rm -", "rf /", "etc");

fn program(k: usize) -> String {
    let f = fanout(32);
    let mut body = String::from("import os\n");
    for _ in 0..k {
        body.push_str(&format!("os.system('{f}')\n"));
    }
    body.push_str(&format!("os.system(\"bash -c '{H}'\")\n"));
    format!("python3 - <<'EOF'\n{body}EOF")
}

#[test]
fn fanout_then_nested_system_harm_in_python_heredoc_is_deny() {
    let mut bad = Vec::new();
    for k in [2usize, 3] {
        let c = program(k);
        for sub in [false, true] {
            let (d, took) = run_as(&c, sub);
            println!("k={k} sub={sub} -> {d} in {}ms", took.as_millis());
            if d != "deny" {
                bad.push(format!("k={k} sub={sub} -> {d}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "system harm not denied:\n{}",
        bad.join("\n")
    );
}

/// Control: with one fan-out call (the total not spent) the same harmful
/// call is Deny in both modes on tip and base.
#[test]
fn control_one_fanout_then_nested_system_harm_is_deny() {
    let c = program(1);
    for sub in [false, true] {
        let (d, _) = run_as(&c, sub);
        assert_eq!(d, "deny", "sub={sub}");
    }
}

/// Same cap, second shape (observed in the same probe): k sequential
/// `python3 -c "import os; os.system('<fan-out N=32>')"` programs, then the
/// tail `bash <(echo '<rm -rf /etc>')`.
///   k=2: base 24263a0c8 main=deny; tip aa0680e6c main=ask (sub deny both)
///   k=3: base 24263a0c8 main=deny; tip aa0680e6c main=ask (sub deny both)
#[test]
fn fanout_programs_then_procsub_system_harm_tail_is_deny() {
    let f = fanout(32);
    let mut bad = Vec::new();
    for k in [2usize, 3] {
        let pad = format!("python3 -c \"import os; os.system('{f}')\"\n").repeat(k);
        let c = format!("{pad}bash <(echo '{H}')");
        for sub in [false, true] {
            let (d, took) = run_as(&c, sub);
            println!("procsub k={k} sub={sub} -> {d} in {}ms", took.as_millis());
            if d != "deny" {
                bad.push(format!("procsub k={k} sub={sub} -> {d}"));
            }
        }
    }
    assert!(
        bad.is_empty(),
        "system harm not denied:\n{}",
        bad.join("\n")
    );
}
