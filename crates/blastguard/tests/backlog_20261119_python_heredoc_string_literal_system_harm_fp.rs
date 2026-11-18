#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (false positive, observed 2026-11-19 on blastguard 0.2.722 by the S4
//! triage subagent): a `python3 - <<'EOF'` program whose body only holds
//! the shell text `exec <<< 'chmod 777 /etc/hosts'; bash script.sh` as a
//! Python STRING LITERAL (it was a Rust test fixture being re-anchored by a
//! string-replacement script) was refused as system harm. Observed reason,
//! verbatim:
//!   "`chmod` re-permissions `/etc/hosts`, which is inside a system
//!    directory — ... so blastguard refuses it [system harm: refused on every
//!    request, never handed back as a question]"
//! Nothing in that command runs `chmod`: the heredoc is quoted, and the text
//! is Python data that is never handed to a shell.
//!
//! Near twins (not this shape): backlog f281b0b1 (`python3 -c` string literal
//! starting with `rm -r`), b424eaed (heredoc, backtick span).
//!
//! Asserted (RED): the string-literal shape is not Deny in main mode.
//! Controls (must stay Deny in main mode): the heredoc body actually running
//! it via `os.system(...)`, and a plain `chmod 777 /etc/hosts`.
//!
//! DOES NOT PROVE: subagent-mode verdicts, other interpreters, unquoted
//! heredocs, or any Python reading beyond these three shapes.
//!
//! RED written by a non-implementer of the gate.

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
        "bg-pyheredoc-syslit-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_main(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "python-heredoc-syslit-fp-20261119",
        "cwd": home.0.join("wt"),
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
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

const STRING_LITERAL: &str = "python3 - <<'EOF'\n\
s = \"exec <<< 'chmod 777 /etc/hosts'; bash script.sh\"\n\
print(len(s))\n\
EOF";

#[test]
#[ignore = "RED: 2026-11-19 shell text inside a Python string literal in a python3 heredoc is refused as system harm — remove the ignore when fixed"]
fn python_heredoc_string_literal_mentioning_system_chmod_is_not_denied() {
    let home = unique_home("red");
    let (d, r) = run_main(STRING_LITERAL, &home);
    assert_ne!(d, "deny", "main reason={r}");
}

#[test]
fn control_python_heredoc_running_system_chmod_stays_deny() {
    let home = unique_home("ctl-run");
    let cmd = "python3 - <<'EOF'\nimport os\nos.system(\"chmod 777 /etc/hosts\")\nEOF";
    let (d, r) = run_main(cmd, &home);
    assert_eq!(d, "deny", "main reason={r}");
}

#[test]
fn control_plain_system_chmod_stays_deny() {
    let home = unique_home("ctl-plain");
    let (d, r) = run_main("chmod 777 /etc/hosts", &home);
    assert_eq!(d, "deny", "main reason={r}");
}
