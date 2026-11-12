#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (fail-open, traced by a verifier; observed against the real hook at
//! fix/bg-python-open-write-modes 3efbbf51, blastguard 0.2.485): Python
//! NFKC-normalises identifiers, so full-width `ｏｐｅｎ(...)` IS `open(...)`.
//! The write-mode detection (detect/interpreter/open_mode.rs) only scans ASCII
//! identifiers, so these writes to a SYSTEM-directory file are not recognised.
//!
//! Shapes: full-width `open`, mixed ASCII/full-width `opｅn`, and a full-width
//! module name (`ｏｓ.chmod`) after `import os`.
//!
//! Asserted: none of the SHAPES is Allow, in main session and subagent.
//! Control (not ignored): ASCII `open('/etc/hosts','a')` is not Allow.
//!
//! DOES NOT PROVE: non-system targets, other interpreters, other Unicode
//! normalisation forms, or that non-Allow shapes are denied for the right reason.

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
        "bg-pynfkc-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "py-nfkc-20261113",
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

fn shapes() -> Vec<String> {
    // U+FF4F U+FF50 U+FF45 U+FF4E = full-width "open"; U+FF4F U+FF53 = "os".
    let fw_open = "\u{ff4f}\u{ff50}\u{ff45}\u{ff4e}";
    let mixed_open = "op\u{ff45}n";
    let fw_os = "\u{ff4f}\u{ff53}";
    vec![
        format!("python3 -c \"{fw_open}('/etc/hosts','a').write('x')\""),
        format!("python3 -c \"{mixed_open}('/etc/hosts','a').write('x')\""),
        format!("python3 -c \"{fw_open}('/etc/hosts','w').write('x')\""),
        format!("python3 -c \"import os; {fw_os}.chmod('/etc/hosts',0)\""),
    ]
}

// test-weakening-justified: new RED for an unfixed fail-open; the ignore is
// removed in the fixing commit.
#[test]
#[ignore = "RED: fail-open, full-width (NFKC) python identifiers on system path not detected"]
fn python_nfkc_identifier_writes_on_system_path_are_not_allowed() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for c in shapes() {
        for subagent in [false, true] {
            let (d, r) = run_as(&c, &home, subagent);
            if d == "allow" {
                failures.push(format!("subagent={subagent} {c:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} NFKC-identifier write(s) to a system path were ALLOWED:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Control: the ASCII shape is not allowed.
#[test]
fn control_ascii_open_append_on_system_path_is_not_allowed() {
    let home = unique_home("ctl");
    for subagent in [false, true] {
        let c = "python3 -c \"open('/etc/hosts','a').write('x')\"";
        let (d, r) = run_as(c, &home, subagent);
        assert_ne!(d, "allow", "subagent={subagent} {c:?} -> {d} | {r}");
    }
}
