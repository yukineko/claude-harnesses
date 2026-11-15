#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false-positive candidate, observed 2026-11-14 in the task worktrees of
//! run-20261114-111444-69657, base b0b4ac6e3 family; minimised by the test
//! author on b0b4ac6e3 / blastguard 0.2.568): in a `python3 - <<'EOF'`
//! program whose write target is a plain string literal, an UNRELATED string
//! literal containing a Markdown backtick span with a `/` in it makes
//! blastguard read that span as the write target and as a shell expansion.
//! Observed reason (main, verbatim prefix):
//!   "`python3 - <<'EOF'` takes its program from stdin, and it writes
//!    ``b/a.rs`` (`,'w')`) — ``b/a.rs`` contains a shell expansion, so
//!    blastguard cannot tell which file this names, ..."
//! Observed (main / subagent), program lines joined by `\n`:
//!   s = "`b/a.rs`"; open('docs/x.md','w').write(s)                  ask   / deny  <- FP candidate
//!   p = "docs/x.md"; s = "`b/a.rs`"; open(p,'w').write(s)           ask   / deny  <- FP candidate
//!   p = "docs/specs/x.md"; s = "`crates/blastguard/tests/a.rs`。"; open(p,'w').write(s)
//!                                                                  ask   / deny  (run-observed shape)
//!   s = "`a.rs`"; open('docs/x.md','w').write(s)                    allow / allow (control: no `/`)
//!   p = "docs/x.md"; s = "b/a.rs"; open(p,'w').write(s)             allow / allow (control: no backtick)
//!
//! Two readings:
//!   (a) defect: the write target is the literal 'docs/x.md' (or p bound to a
//!       literal); the backtick span is inside a Python string and is never
//!       shell-expanded (the heredoc is quoted), so Ask/Deny is a false positive.
//!   (b) intended: blastguard deliberately refuses when it cannot parse the
//!       Python program. The reason, however, names the wrong file
//!       (``b/a.rs``) as the write target, which points to (a).
//!
//! Asserted: the three FP shapes are Allow in main and subagent mode.
//! Control: the no-slash and no-backtick variants are Allow.
//!
//! DOES NOT PROVE: other writer forms (`Path.write_text`, `with open`), other
//! interpreters, unquoted heredocs, or that a genuine shell expansion in the
//! write target stays Ask.
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
        "bg-pybacktickfp-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "python-heredoc-backtick-fp-20261114",
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
#[ignore = "RED: 2026-11-14 backtick path span in an unrelated Python string literal is misread as the write target — remove the ignore when fixed"]
fn backtick_span_in_unrelated_python_string_is_not_the_write_target() {
    let cmds = [
        "python3 - <<'EOF'\ns = \"`b/a.rs`\"\nopen('docs/x.md','w').write(s)\nEOF",
        "python3 - <<'EOF'\np = \"docs/x.md\"\ns = \"`b/a.rs`\"\nopen(p,'w').write(s)\nEOF",
        "python3 - <<'EOF'\np = \"docs/specs/x.md\"\ns = \"`crates/blastguard/tests/a.rs`\u{3002}\"\nopen(p,'w').write(s)\nEOF",
    ];
    let bad = unallowed(&cmds, "red");
    assert!(
        bad.is_empty(),
        "expected Allow for every shape:\n{}",
        bad.join("\n")
    );
}

#[test]
fn control_python_heredoc_without_slash_backtick_span_is_allow() {
    let cmds = [
        "python3 - <<'EOF'\ns = \"`a.rs`\"\nopen('docs/x.md','w').write(s)\nEOF",
        "python3 - <<'EOF'\np = \"docs/x.md\"\ns = \"b/a.rs\"\nopen(p,'w').write(s)\nEOF",
    ];
    let bad = unallowed(&cmds, "ctl");
    assert!(
        bad.is_empty(),
        "control shapes must stay Allow:\n{}",
        bad.join("\n")
    );
}
