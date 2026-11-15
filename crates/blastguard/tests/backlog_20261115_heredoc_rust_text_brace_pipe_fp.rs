#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, side finding F16 observed 2026-11-15 on base 0.2.603):
//! a Bash call `cd crates/blastguard && python3 - <<'EOF' ... EOF` whose
//! quoted-heredoc body is a Python program doing `str.replace` on a `.rs` file
//! was blocked (subagent deny) with: "`mut Vec<String>` is piped into the
//! brace group `{ …`, whose closing `}` blastguard cannot find — it cannot
//! tell which command inside reads the piped program". The Rust closure
//! `|n: u32, what: &str, bodies: &mut Vec<String>, why: &mut Vec<String>| {`
//! sat inside a Python TRIPLE-QUOTED string, and its replacement was a
//! one-line closure `{ body(n, what, bodies, why) }`.
//!
//! The designer's reduction (observed at HEAD): the triple-quoted replace
//! below reproduces the reason verbatim (main ask, subagent deny); the same
//! closure inside an ordinary one-line Python string is allow/allow, so the
//! trigger is the triple-quoted multi-line string.
//!
//! Two readings: (1) real defect: the heredoc is quoted (`<<'EOF'`) so its
//! body is data for `python3 -`; the Rust text is inside Python strings and
//! the only effect is writing one `.rs` file in the working tree; the shell
//! never sees the `|` or `{`; the verdict must be allow; (2) intended: the
//! body is read as shell when the Python strings cannot be tokenised, so a
//! `| {` is conservatively undeterminable. Tell apart: a quoted heredoc
//! performs no shell parsing of its body, and the one-line-string twin is
//! allow, so the only difference is the Python-level string spelling ->
//! reading (1).
//!
//! Asserted: allow in main and subagent mode (it is a harmless write inside
//! the cwd; intended verdict confirmed by main 2026-11-15: a python heredoc
//! body is not shell and a Rust closure inside a Python string is data).
//!
//! DOES NOT PROVE: heredocs for other interpreters, a body that really
//! pipes in Python (`os.system("a | { b; }")`), or the exact original
//! command (it was not available to the designer). Controls: a body whose
//! Python really runs system harm stays deny; the one-line-string twin is
//! allow.
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
        "bg-side1115-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "side-findings-20261115",
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

/// Verified at HEAD: main ask / subagent deny (the F16 reason, verbatim).
const TRIPLE_QUOTED_REPLACE: &str = r##"cd crates/blastguard && python3 - <<'EOF'
import re
f = "tests/x.rs"
s = open(f).read()
s = s.replace("""        let from_exec = |n: u32, what: &str, bodies: &mut Vec<String>, why: &mut Vec<String>| {
            body(n, what, bodies, why);
        };""", """        let from_exec = |n: u32, what: &str, bodies: &mut Vec<String>, why: &mut Vec<String>| { body(n, what, bodies, why) };""")
open(f, "w").write(s)
print("ok")
EOF"##;

/// Verified at HEAD: allow/allow.
const ONE_LINE_STRING_TWIN: &str = r##"python3 - <<'EOF'
src = "let f = |bodies: &mut Vec<String>| { x };"
open("t.rs","w").write(src)
EOF"##;

/// Verified at HEAD: deny/deny (the Python really runs recursive rm of /).
const REAL_HARM_BODY: &str = r##"python3 - <<'EOF'
import os; os.system("rm -rf /")
EOF"##;

#[test]
#[ignore = "RED: 2026-11-15 F16 triple-quoted Rust closure in quoted python heredoc misread as pipe into brace group — remove when fixed"]
fn heredoc_python_replace_with_rust_closure_in_triple_quote_is_allow() {
    let home = unique_home("a");
    let (m, mr) = run_as(TRIPLE_QUOTED_REPLACE, &home, false);
    let (s, sr) = run_as(TRIPLE_QUOTED_REPLACE, &home, true);
    assert_eq!(m, "allow", "main; reason={mr}");
    assert_eq!(s, "allow", "subagent; reason={sr}");
}

#[test]
fn control_one_line_string_closure_is_allow() {
    let home = unique_home("twin");
    let (m, mr) = run_as(ONE_LINE_STRING_TWIN, &home, false);
    let (s, sr) = run_as(ONE_LINE_STRING_TWIN, &home, true);
    assert_eq!(m, "allow", "main; reason={mr}");
    assert_eq!(s, "allow", "subagent; reason={sr}");
}

#[test]
fn control_python_heredoc_running_system_harm_stays_deny() {
    let home = unique_home("harm");
    let (m, mr) = run_as(REAL_HARM_BODY, &home, false);
    let (s, sr) = run_as(REAL_HARM_BODY, &home, true);
    assert_eq!(m, "deny", "main; reason={mr}");
    assert_eq!(s, "deny", "subagent; reason={sr}");
}
