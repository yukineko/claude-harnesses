//! RED (false positive; verbatim relay replay; independent test author,
//! 2026-11-22). Observed in condukt run run-20261122-020202-63344 (main
//! 6ae161ee7 era), relay bgr-05b37d8cbc85 (subagent, Deny). The command was
//!   cd <task worktree> && python3 - <<'EOF' ... p='crates/tdd/src/evidence.rs'
//!   ... s=open(p).read() ... (string splicing with '''...''' / r'''...'''
//!   literals holding Rust source with backticks) ... open(p,'w').write(s)
//!   EOF
//!   . "$HOME/.cargo/env"; cargo check -p tdd 2>&1 | tail -20
//! Verbatim reason (prefix): "> overwrites , which names command-running
//! configuration (a hook wiring file, a plugin hook, a gate script) or cannot
//! be placed and so may name it ..." -- the write target is EMPTY in the
//! reason, while the program binds `p` to the literal 'crates/tdd/src/evidence.rs'
//! (an ordinary Rust source file, not command-running configuration). The
//! heredoc is quoted, so nothing in the body is shell-expanded.
//!
//! The fixture `support/relay_bgr_05b37d8cbc85_20261122_exact.txt` is the exact
//! relayed command; the only rewrite is the `cd` target (the task worktree
//! path) -> this test's cwd, so the relative write lands inside the session
//! cwd.
//!
//! Readings: (a) FP -- the target is text-determined (memory
//! blastguard-must-resolve-statically; 2026-11-20 round-2 ruling: an
//! allowlisted fs-write sink is judged on its resolved path) and names an
//! ordinary .rs file; (b) intended -- the Python program's string splicing is
//! beyond the supported Python subset, so the write target is unresolvable and
//! Ask (subagent Deny) is the ruling's "Unresolvable arguments -> Ask". Even
//! under (b) the reason misnames the class (an empty path "which names
//! command-running configuration"). Tell apart: whether blastguard's Python
//! reader resolves `p` here (the control below, same `p` binding without the
//! splicing body, is expected Allow).
//!
//! DOES NOT PROVE: which part of the body triggers it (not minimised); the
//! original cd-into-another-worktree classification; real Python execution.
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

#[path = "support/bounded.rs"]
mod bounded;

use std::process::{Command, Stdio};

const EXACT: &str = include_str!("support/relay_bgr_05b37d8cbc85_20261122_exact.txt");
const ORIG_CD: &str = "/Users/yuki/.condukt/worktrees/run-20261122-020202-63344-a867";

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
        "bg-relay05b37-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "relay-05b37d8cbc85-20261122",
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

fn unallowed(cmd: &str, tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let wt = home.0.join("wt");
    let cmd = cmd.replace(ORIG_CD, wt.to_str().unwrap());
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        if d != "allow" {
            bad.push(format!("subagent={subagent} => {d} ({r})"));
        }
    }
    bad
}

#[test]
#[ignore = "RED(8ff44387): verbatim relay bgr-05b37d8cbc85 python3 heredoc write to a literal .rs path is refused with an empty target"]
fn relay_05b37d8cbc85_exact_command_is_allow() {
    assert!(EXACT.contains(ORIG_CD), "fixture: cd target present");
    let bad = unallowed(EXACT.trim_end_matches('\n'), "exact");
    assert!(
        bad.is_empty(),
        "expected Allow for the verbatim relayed command:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_same_p_binding_without_splicing_body_is_allow() {
    let cmd = format!(
        "cd {ORIG_CD} && python3 - <<'EOF'\np='crates/tdd/src/evidence.rs'\ns=open(p).read()\nopen(p,'w').write(s)\nEOF\n. \"$HOME/.cargo/env\"; cargo check -p tdd 2>&1 | tail -20"
    );
    let bad = unallowed(&cmd, "ctl");
    assert!(
        bad.is_empty(),
        "control expected Allow:\n{}",
        bad.join("\n")
    );
}
