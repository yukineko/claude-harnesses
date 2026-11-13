#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (observed 2026-11-13, independent verifier at fix branch 02553aa8):
//! python-level backslash + real LF ("BN") inside a non-raw string literal.
//! Python removes the BN, so `'/etc/ho\<LF>sts'` is `/etc/hosts` at runtime,
//! but blastguard does not see the system path.
//!
//! P1: BN inside a double-quoted python literal within a single-quoted shell arg.
//! P2/P3: shell turns `\\` into `\` inside double quotes, python sees `ho\<LF>sts`.
//!
//! Asserted (RED): none of P1-P3 is Allow, in the main session and a subagent.
//! Control (not ignored): the same programs with `/etc/hosts` written plainly
//! are not Allow. Negative control (printed, NOT asserted): a raw string
//! `r'/etc/ho\<LF>sts'`, where python keeps the backslash and the path is not
//! /etc/hosts.
//!
//! DOES NOT PROVE: which stage keeps the BN; that other literal shapes are safe.

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
        "bg-psl-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-string-literal-bn-20261113",
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

/// (label, command with BN in the literal, same program with plain /etc/hosts).
const SHAPES: &[(&str, &str, &str)] = &[
    (
        "P1-single-quoted-shell-arg",
        "python3 -c 'open(\"/etc/ho\\\nsts\",\"w\")'",
        "python3 -c 'open(\"/etc/hosts\",\"w\")'",
    ),
    (
        "P2-double-quoted-shell-arg",
        "python3 -c \"open('/etc/ho\\\\\nsts','w')\"",
        "python3 -c \"open('/etc/hosts','w')\"",
    ),
    (
        "P3-assigned-variable",
        "python3 -c \"x='/etc/ho\\\\\nsts'; open(x,'w')\"",
        "python3 -c \"x='/etc/hosts'; open(x,'w')\"",
    ),
];

/// Raw python string: python keeps the backslash, so the path is NOT /etc/hosts.
const RAW_NEGATIVE: &str = "python3 -c \"open(r'/etc/ho\\\\\nsts','w')\"";

// test-weakening-justified: new RED for an unfixed fail-open family
// (2026-11-13); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: python string-literal backslash-LF is removed by python, hiding /etc/hosts from blastguard"]
fn backslash_newline_in_python_string_literal_is_not_allowed() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for (label, bn, _) in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(bn, &home, subagent);
            println!("[{label}] subagent={subagent} {bn:?} -> {d} | {r}");
            if d == "allow" {
                failures.push(format!("[{label}] subagent={subagent} {bn:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} of {} (shape x mode) BN-in-literal runs were ALLOWED:\n{}",
        failures.len(),
        SHAPES.len() * 2,
        failures.join("\n")
    );
}

// Control: plain /etc/hosts is not Allow.
#[test]
fn control_same_programs_with_plain_path_are_not_allowed() {
    let home = unique_home("ctl");
    let mut failures = Vec::new();
    for (label, _, plain) in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(plain, &home, subagent);
            println!("[{label}] subagent={subagent} {plain:?} -> {d} | {r}");
            if d == "allow" {
                failures.push(format!(
                    "[{label}] subagent={subagent} {plain:?} -> {d} | {r}"
                ));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} run(s) were ALLOWED even with a plain path:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Negative control: printed only, deliberately not asserted.
#[test]
fn negative_control_raw_string_decision_is_printed() {
    let home = unique_home("neg");
    for subagent in [false, true] {
        let (d, r) = run_as(RAW_NEGATIVE, &home, subagent);
        println!("[raw-string] subagent={subagent} {RAW_NEGATIVE:?} -> {d} | {r}");
    }
}
