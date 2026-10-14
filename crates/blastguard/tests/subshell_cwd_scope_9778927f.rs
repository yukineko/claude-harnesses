#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 9778927f follow-up: a `cd` inside a subshell must not leak out.
//!
//! blastguard tracks `cd` across the segments of one command line so that a
//! relative operand is judged against the directory it will really resolve
//! in. A `cd` inside `( … )` (or inside `$( … )`) runs in a child shell and
//! does NOT change the directory of the segments after the closing paren.
//! Required property: each line containing such a subshell gets exactly the
//! same verdict as the same line with the subshell removed entirely.
//!
//! Every command below is only passed as a STRING to the blastguard hook via
//! its PreToolUse JSON on stdin. Nothing is executed.

use std::io::Write;
use std::process::{Command, Stdio};

fn bin() -> &'static str {
    env!("CARGO_BIN_EXE_blastguard")
}

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
        "bg-9778927f-cwd-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Run the hook as an INTERACTIVE session (so an Ask stays an Ask);
/// returns (decision, reason).
fn verdict(command: &str) -> (String, String) {
    let home = unique_home("v");
    let payload = serde_json::json!({
        "session_id": "bg-9778927f-cwd",
        "cwd": env!("CARGO_MANIFEST_DIR"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    })
    .to_string();
    let mut child = Command::new(bin())
        .env_clear()
        .env("HOME", &home.0)
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(payload.as_bytes())
        .unwrap();
    let out = child.wait_with_output().unwrap();
    assert!(
        out.status.success(),
        "blastguard exited {:?} for {command:?}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
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

/// For each (line, baseline) pair: the line must get the SAME decision as the
/// baseline (the line with the subshell removed). All violations at once.
fn assert_same_as_baseline(cases: &[(&str, &str)]) {
    let mut failures = Vec::new();
    for (line, baseline) in cases {
        let (bd, br) = verdict(baseline);
        let (d, r) = verdict(line);
        if d != bd {
            failures.push(format!(
                "{line:?} => {d} | {r:?}\n    baseline {baseline:?} => {bd} | {br:?}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "a `cd` inside a subshell changed the verdict of segments outside it:\n  {}",
        failures.join("\n  ")
    );
}

/// Controls: the subshell-free baselines used below have the verdicts the
/// defect report observed. If these move, the comparison tests mean nothing.
#[test]
fn baselines_have_expected_verdicts() {
    let cases: &[(&str, &str)] = &[
        ("cd /etc && echo x > hosts", "deny"),
        ("cd /usr && rm -rf lib", "deny"),
        ("cd .githooks && rm pre-commit", "deny"),
    ];
    let mut failures = Vec::new();
    for (cmd, want) in cases {
        let (d, r) = verdict(cmd);
        if d != *want {
            failures.push(format!("{cmd:?} expected {want}, got {d} | {r:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "baseline controls drifted:\n  {}",
        failures.join("\n  ")
    );
}

#[test]
fn cd_inside_subshell_does_not_move_outer_redirect() {
    assert_same_as_baseline(&[
        (
            "cd /etc && ( cd /tmp ) && echo x > hosts",
            "cd /etc && echo x > hosts",
        ),
        (
            "cd /etc && (cd /tmp) && echo x > hosts",
            "cd /etc && echo x > hosts",
        ),
        (
            "cd /etc && ( cd /tmp; true ) && echo x > hosts",
            "cd /etc && echo x > hosts",
        ),
        (
            "cd /etc; (cd /tmp); echo x > hosts",
            "cd /etc; echo x > hosts",
        ),
        (
            "cd /etc\n(\ncd /tmp\n)\necho x > hosts",
            "cd /etc\necho x > hosts",
        ),
        (
            "cd /etc\n(cd /tmp)\necho x > hosts",
            "cd /etc\necho x > hosts",
        ),
    ]);
}

#[test]
fn cd_inside_subshell_does_not_move_outer_rm() {
    assert_same_as_baseline(&[
        (
            "cd .githooks && ( cd /tmp ) && rm pre-commit",
            "cd .githooks && rm pre-commit",
        ),
        (
            "cd .githooks && (cd /tmp) && rm pre-commit",
            "cd .githooks && rm pre-commit",
        ),
        (
            "cd /usr && ( cd /tmp ) && rm -rf lib",
            "cd /usr && rm -rf lib",
        ),
        (
            "cd /usr && (cd /tmp) && rm -rf lib",
            "cd /usr && rm -rf lib",
        ),
        (
            "cd /usr && ( cd /tmp; true ) && rm -rf lib",
            "cd /usr && rm -rf lib",
        ),
        ("cd /usr\n(\ncd /tmp\n)\nrm -rf lib", "cd /usr\nrm -rf lib"),
    ]);
}

#[test]
fn cd_inside_command_substitution_does_not_move_outer_redirect() {
    let baseline = "cd /etc && echo x > hosts";
    let (bd, br) = verdict(baseline);
    assert_eq!(bd, "deny", "control {baseline:?} => {bd} | {br:?}");
    let mut failures = Vec::new();
    for line in [
        "cd /etc && echo $(cd /tmp; pwd) && echo x > hosts",
        "cd /etc && echo $(cd /tmp && pwd) && echo x > hosts",
        "cd /etc && X=$(cd /tmp; pwd) && echo x > hosts",
    ] {
        let (d, r) = verdict(line);
        if d == "allow" || d != bd {
            failures.push(format!(
                "{line:?} => {d} | {r:?} (baseline {baseline:?} => {bd})"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "a `cd` inside $( ) changed the verdict of segments outside it:\n  {}",
        failures.join("\n  ")
    );
}

/// Non-regression: a `cd` inside the subshell still applies INSIDE it.
#[test]
fn cd_inside_subshell_still_applies_inside_it() {
    assert_same_as_baseline(&[
        ("(cd /tmp && rm -rf lib)", "cd /tmp && rm -rf lib"),
        ("( cd /tmp && rm -rf lib )", "cd /tmp && rm -rf lib"),
        ("(cd /usr && rm -rf lib)", "cd /usr && rm -rf lib"),
    ]);
    // The inner-subshell /usr case must stay deny (cd inside applies inside).
    let (d, r) = verdict("(cd /usr && rm -rf lib)");
    assert_eq!(d, "deny", "(cd /usr && rm -rf lib) => {d} | {r:?}");
}

/// Non-regression: benign subshell idioms stay allow.
#[test]
fn benign_subshell_cd_stays_allow() {
    let mut failures = Vec::new();
    for line in [
        "(cd sub && cargo test) && ls",
        "( cd sub && cargo test ) && ls",
        "(cd /tmp) && ls",
        "cd sub && (cd inner && cargo build) && cargo test",
    ] {
        let (d, r) = verdict(line);
        if d != "allow" {
            failures.push(format!("{line:?} => {d} | {r:?}"));
        }
    }
    assert!(
        failures.is_empty(),
        "benign subshell lines no longer allow:\n  {}",
        failures.join("\n  ")
    );
}
