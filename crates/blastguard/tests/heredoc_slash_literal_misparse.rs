#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED regression: a slash-slash Python string literal inside a quoted-delimiter
//! heredoc body is read as a write target named `//`.
//!
//! Observed 2026-10-15: a command of the shape
//!
//! ```text
//! cd <repo>/crates/backlog/tests && python3 - <<'EOF'
//! def drop(p, names):
//!     s=open(p).read()
//!     ok = all(l.strip().startswith('//') for l in s.splitlines())
//!     open(p,'w').write(s)
//! drop('cancel_evidence.rs',['x'])
//! EOF
//! ```
//!
//! was refused with "... it writes `//` (`,'w')`) — it exists, and it is not
//! inside a git work tree ...". The `'//'` is a string literal; the real write
//! target is `p`. The same script without that literal is allowed (control).
//!
//! End-to-end against the built binary, same harness as
//! `heredoc_quoted_body_redirect_false_positive.rs`.

use std::io::Write;
use std::process::{Command, Stdio};

fn verdict(command: &str) -> (String, String) {
    let bin = env!("CARGO_BIN_EXE_blastguard");
    let payload = serde_json::json!({
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": { "command": command },
    })
    .to_string();
    let mut child = Command::new(bin)
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env_remove("CLAUDE_CODE_SESSION_ID")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("binary spawns");
    if let Some(mut stdin) = child.stdin.take() {
        let _ = stdin.write_all(payload.as_bytes());
    }
    let out = child.wait_with_output().expect("binary runs");
    assert_eq!(out.status.code(), Some(0), "hook must exit 0 ({command:?})");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    if stdout.trim().is_empty() {
        return ("allow".to_string(), String::new());
    }
    let doc: serde_json::Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("stdout not JSON for {command:?} ({e}): {stdout}"));
    let hso = doc.get("hookSpecificOutput");
    let decision = hso
        .and_then(|h| h.get("permissionDecision"))
        .and_then(|v| v.as_str())
        .unwrap_or_else(|| panic!("no permissionDecision for {command:?}: {stdout}"))
        .to_string();
    let reason = hso
        .and_then(|h| h.get("permissionDecisionReason"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    (decision, reason)
}

/// A real git work tree containing `crates/backlog/tests/cancel_evidence.rs`.
fn repo_tests_dir(tag: &str) -> String {
    let root = std::env::temp_dir().join(format!(
        "blastguard-slashlit-{}-{}",
        std::process::id(),
        tag
    ));
    let _ = std::fs::remove_dir_all(&root);
    let tests = root.join("crates/backlog/tests");
    std::fs::create_dir_all(&tests).unwrap();
    std::fs::write(tests.join("cancel_evidence.rs"), "// x\n").unwrap();
    let st = Command::new("git")
        .arg("init")
        .arg("-q")
        .arg(&root)
        .status()
        .expect("git init");
    assert!(st.success(), "git init must succeed");
    tests.to_string_lossy().into_owned()
}

fn script(dir: &str, literal: &str) -> String {
    format!(
        "cd {dir} && python3 - <<'EOF'\n\
def drop(p, names):\n\
    s=open(p).read()\n\
    ok = all(l.strip().startswith('{literal}') for l in s.splitlines())\n\
    open(p,'w').write(s)\n\
drop('cancel_evidence.rs',['x'])\n\
EOF"
    )
}

fn assert_not_denied_for_slash(command: &str) {
    let (decision, reason) = verdict(command);
    assert!(
        !(decision == "deny" && reason.contains("`//`")),
        "false positive: string literal treated as a write target.\n\
         decision={decision}\nreason={reason}"
    );
}

#[test]
#[ignore = "RED: heredoc-body string literal slash-slash misparsed as write target `//`; remove ignore when fixed"]
fn slash_string_literal_in_python_heredoc_is_not_a_write_target() {
    let dir = repo_tests_dir("red");
    assert_not_denied_for_slash(&script(&dir, "//"));
}

#[test]
fn control_same_script_without_slash_literal_is_not_denied_for_slash() {
    let dir = repo_tests_dir("ctl");
    assert_not_denied_for_slash(&script(&dir, "#"));
}
