#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false-positive candidate, observed 2026-11-14 in the task worktrees of
//! run-20261114-111444-69657, base b0b4ac6e3 family; re-observed by the test
//! author on b0b4ac6e3 / blastguard 0.2.568): the body of a QUOTED
//! here-document (`<<'EOF'`) is literal data — bash performs no expansion in
//! it — yet a `$(f)` in that body, where `f` is a harmless function defined
//! earlier in the same command, is Ask in main / Deny in subagent with
//! "the command word `$(f)` is an expansion whose value only exists at run
//! time — blastguard cannot tell what program this runs".
//! Observed (main / subagent):
//!   f() { echo hi; }; cat <<'EOF'\n$(f)\nEOF        ask   / deny  <- FP candidate
//!   f() { echo hi; }; cat <<EOF\n$(f)\nEOF          ask   / deny  (unquoted: body IS expanded; not asserted)
//!   cat <<'EOF'\n$(echo hi)\nEOF                     allow / allow (control)
//!   cat <<'EOF'\nhello\nEOF                          allow / allow (control)
//!
//! Two readings:
//!   (a) defect: the quoted body is never executed, so `$(f)` there is text;
//!       judging it as a command word is a false positive.
//!   (b) intended: blastguard may deliberately judge heredoc bodies
//!       conservatively when a function is in scope. The control
//!       `cat <<'EOF'\n$(echo hi)\nEOF` being Allow suggests the quoted body is
//!       otherwise treated as data, so (a) looks more likely, but this test
//!       does not decide it.
//!
//! Asserted: the quoted-heredoc shape is Allow in main and subagent mode.
//! Control: quoted heredocs without a function in scope are Allow.
//!
//! DOES NOT PROVE: anything about the unquoted `<<EOF` shape (deliberately not
//! asserted — its body is expanded, so Ask there may be correct), `<<"EOF"` /
//! `<<\EOF` quoting forms, or `<<-'EOF'`.
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
        "bg-qheredocfp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "quoted-heredoc-subst-fp-20261114",
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

fn unallowed(cmds: &[&str], tag: &str) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if d != "allow" {
                bad.push(format!("subagent={subagent} {cmd:?} => {d} ({r})"));
            }
        }
    }
    bad
}

#[test]
#[ignore = "RED: 2026-11-14 $(f) inside a quoted <<'EOF' body (literal data) is Ask/Deny when f is defined — remove the ignore when fixed"]
fn function_substitution_text_in_quoted_heredoc_body_is_allow() {
    let cmds = ["f() { echo hi; }; cat <<'EOF'\n$(f)\nEOF"];
    let bad = unallowed(&cmds, "red");
    assert!(
        bad.is_empty(),
        "expected Allow for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_quoted_heredoc_without_function_in_scope_is_allow() {
    let cmds = ["cat <<'EOF'\n$(echo hi)\nEOF", "cat <<'EOF'\nhello\nEOF"];
    let bad = unallowed(&cmds, "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Allow:\n{}",
        bad.join("\n")
    );
}
