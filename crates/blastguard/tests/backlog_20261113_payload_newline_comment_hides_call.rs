#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (CRITICAL fail-open, observed 2026-11-13 at main d37ae072): a `#`
//! comment anywhere BEFORE a system-harm call in a multi-line `python3 -c`
//! program turns the Deny into Allow.
//!
//! `python3 -c "import os\nos.chmod('/etc/hosts',0)"` is denied as system harm
//! ("the program calls `os.chmod` on `/etc/hosts` ..."), but
//! `python3 -c "import os # x\nos.chmod('/etc/hosts',0)"` is ALLOWED — in the
//! main session and in a subagent. Python ends a `#` comment at the newline and
//! runs `os.chmod`; blastguard does not see the call.
//!
//! Suspected cause (not proven by this test): `interpreter_payloads_after`
//! (crates/blastguard/src/detect/payload.rs) rebuilds the program from
//! whitespace-split tokens joined by a space, so every newline becomes a space;
//! the Python lexer used by `program_call_verdict`
//! (detect/interpreter/shell_calls.rs) then drops everything after the `#` to
//! the end of the (now single) line, including the call.
//!
//! Asserted: every program below, each of which Python would execute up to the
//! harmful call, is not Allow. Controls (GREEN at HEAD, not ignored): the same
//! programs without the comment are denied, so the RED is attributable to the
//! comment arrangement alone.
//!
//! DOES NOT PROVE: which internal stage loses the newline; anything about
//! `perl`/`ruby`/`node` (their comment-only mentions reached Ask, not Allow, in
//! the same probe); nor the separate observation that `open('/etc/hosts','a')`
//! (append mode) is Allow with or without a comment.

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
        "bg-nlc-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "newline-comment-20261113",
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

/// (with-comment program, same program without the comment).
const SHAPES: &[(&str, &str)] = &[
    // trailing comment on the line before the call
    (
        "python3 -c \"import os # x\nos.chmod('/etc/hosts',0)\"",
        "python3 -c \"import os\nos.chmod('/etc/hosts',0)\"",
    ),
    // comment on its own line
    (
        "python3 -c \"import os\n# x\nos.chmod('/etc/hosts',0)\"",
        "python3 -c \"import os\nos.chmod('/etc/hosts',0)\"",
    ),
    // dedent-sensitive: comment inside an indented block, call dedented after it
    (
        "python3 -c \"import os\nif True:\n    pass # x\nos.chmod('/etc/hosts',0)\"",
        "python3 -c \"import os\nif True:\n    pass\nos.chmod('/etc/hosts',0)\"",
    ),
    // comment on the block opener, call inside the block
    (
        "python3 -c \"import os\nif True: # x\n    os.chmod('/etc/hosts',0)\"",
        "python3 -c \"import os\nif True:\n    os.chmod('/etc/hosts',0)\"",
    ),
    // a different mutator and a different system path
    (
        "python3 -c \"import os # x\nos.rename('/etc/hosts','/tmp/h')\"",
        "python3 -c \"import os\nos.rename('/etc/hosts','/tmp/h')\"",
    ),
    (
        "python3 -c \"import os # x\nos.chmod('/usr/bin/true',0)\"",
        "python3 -c \"import os\nos.chmod('/usr/bin/true',0)\"",
    ),
];

// test-weakening-justified: new RED for an unfixed CRITICAL fail-open (backlog
// pending, 2026-11-13); the ignore is removed in the fixing commit.
#[test]
#[ignore = "RED: <backlog-pending> 2026-11-13 python -c newline collapse lets a # comment hide a system-harm call — remove the ignore when fixed"]
fn comment_before_system_harm_call_does_not_allow_it() {
    let home = unique_home("red");
    let mut failures = Vec::new();
    for (with_comment, _) in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(with_comment, &home, subagent);
            if d == "allow" {
                failures.push(format!("subagent={subagent} {with_comment:?} -> {d} | {r}"));
            }
        }
    }
    assert!(
        failures.is_empty(),
        "{} program(s) whose system-harm call follows a `#` comment were ALLOWED:\n{}",
        failures.len(),
        failures.join("\n")
    );
}

// Control: without the comment every program is denied.
#[test]
fn control_same_programs_without_comment_are_denied() {
    let home = unique_home("ctl");
    for (_, without) in SHAPES {
        for subagent in [false, true] {
            let (d, r) = run_as(without, &home, subagent);
            assert_eq!(d, "deny", "subagent={subagent} {without:?} -> {d} | {r}");
        }
    }
}
