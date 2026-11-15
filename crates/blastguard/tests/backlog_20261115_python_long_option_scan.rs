#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Reshaped 2026-11-15 after observation: `python_program_source` already
//! returns Undetermined on any long option, so the `--xxx` break in
//! `python_reads_stdin_program` is unreachable end-to-end. The observable defect
//! is the false positive: `echo 'print(1)' | python3 --check-hash-based-pycs
//! always x.py` is Deny in subagent. RED tests assert that known long-option
//! spellings are judged like plain `python3 x.py`; GUARDS (non-ignored) keep
//! `-i` / `-m code` after a long option Deny/not-Allow.
//! User ruling 2026-11-15: "長いoptionでも止めない" - the scan must not stop at
//! long options. `--check-hash-based-pycs MODE` (or `=MODE`) takes a value;
//! `--help*` / `--version` take none; an unknown long option is undetermined and
//! must resolve to the restrictive side (counts as reading stdin).
//!
//! P = `echo 'import os; os.chmod("/etc/hosts", 0o777)' | `
//!   P python3 --check-hash-based-pycs always -i x.py   expected Deny/Deny
//!   P python3 --check-hash-based-pycs=always -i x.py   expected Deny/Deny
//!   P python3 --some-unknown-long-opt -i x.py          expected not Allow
//! Controls (hold today): P python3 -i x.py is Deny/Deny;
//!   `echo 'print(1)' | python3 --check-hash-based-pycs always x.py` is not Deny.
//!
//! DOES NOT PROVE: other interpreters, `-m` module variants after a long option
//! beyond what is listed here, or that every unknown long option is handled.
//!
//! RED written by an independent test author, not the implementer.

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
        "bg-pylongopt-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-long-option-scan-20261115",
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

const PY: &str = "echo 'import os; os.chmod(\"/etc/hosts\", 0o777)' | ";

fn undenied(cmds: &[String], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != "deny" {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

// GUARDS (pass today; NOT RED). `python_program_source` returns Undetermined on
// any long option today, so these Deny via that path. They exist to catch a fix
// that models the long option in the first walker but leaves
// `python_reads_stdin_program` stopping at it, which would drop a later `-i`/`-m`.
#[test]
fn guard_long_option_with_separate_value_then_i_is_deny() {
    let bad = undenied(
        &[format!(
            "{PY}python3 --check-hash-based-pycs always -i x.py"
        )],
        "sep",
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn guard_long_option_with_equals_value_then_i_is_deny() {
    let bad = undenied(
        &[format!(
            "{PY}python3 --check-hash-based-pycs=always -i x.py"
        )],
        "eq",
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

#[test]
fn guard_unknown_long_option_then_i_is_not_allow() {
    let home = unique_home("unk");
    let cmd = format!("{PY}python3 --some-unknown-long-opt -i x.py");
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = run_as(&cmd, &home, subagent);
        if d == "allow" {
            bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
        }
    }
    assert!(bad.is_empty(), "expected not Allow:\n{}", bad.join("\n"));
}

#[test]
fn control_harmful_program_piped_to_python_i_is_deny() {
    let bad = undenied(&[format!("{PY}python3 -i x.py")], "ctl-i");
    assert!(
        bad.is_empty(),
        "control shapes must stay Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn guard_long_option_then_m_code_is_deny() {
    let bad = undenied(
        &[format!(
            "{PY}python3 --check-hash-based-pycs always -m code"
        )],
        "m-code",
    );
    assert!(bad.is_empty(), "expected Deny:\n{}", bad.join("\n"));
}

const BENIGN: &str = "echo 'print(1)' | ";

fn verdicts(cmd: &str, tag: &str) -> Vec<(bool, String, String)> {
    let home = unique_home(tag);
    [false, true]
        .into_iter()
        .map(|s| {
            let (d, r) = run_as(cmd, &home, s);
            (s, d, r)
        })
        .collect()
}

#[test]
fn control_benign_pipe_to_python_script_is_not_deny() {
    let cmd = format!("{BENIGN}python3 x.py");
    let bad: Vec<_> = verdicts(&cmd, "ctl-benign")
        .into_iter()
        .filter(|(_, d, _)| d == "deny")
        .map(|(s, d, r)| format!("subagent={s} {cmd:?} => {d} ({r})"))
        .collect();
    assert!(bad.is_empty(), "expected not Deny:\n{}", bad.join("\n"));
}

fn assert_same_as_plain(long_spelling: &str, tag: &str) {
    let plain_cmd = format!("{BENIGN}python3 x.py");
    let long_cmd = format!("{BENIGN}python3 {long_spelling} x.py");
    let plain = verdicts(&plain_cmd, &format!("{tag}-plain"));
    let long = verdicts(&long_cmd, tag);
    let bad: Vec<_> = plain
        .iter()
        .zip(long.iter())
        .filter(|(p, l)| p.1 != l.1)
        .map(|(p, l)| {
            format!(
                "subagent={} plain {plain_cmd:?} => {} but {long_cmd:?} => {} ({})",
                p.0, p.1, l.1, l.2
            )
        })
        .collect();
    assert!(bad.is_empty(), "verdicts differ:\n{}", bad.join("\n"));
}

#[test]
fn known_long_option_separate_value_judged_like_plain_script() {
    assert_same_as_plain("--check-hash-based-pycs always", "fp-sep");
}

#[test]
fn known_long_option_equals_value_judged_like_plain_script() {
    assert_same_as_plain("--check-hash-based-pycs=always", "fp-eq");
}
