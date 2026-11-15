#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (3da14bbd follow-up, written 2026-11-16 by the coordinator, who did not
//! write the fix; the hole was found by an independent verifier reviewing
//! commit 2503b9bef):
//!
//! The 3da14bbd fix judges a literal `os.system("...")` / `os.popen("...")`
//! command line by its words. It reads a stdin-reading child (shell, code
//! interpreter, REPL) as making piped stdin a program, and other literal
//! commands as keeping stdin as data. These literals start `bash` at run time
//! without spelling `bash` in their words:
//!
//!   `"\N{LATIN SMALL LETTER B}ash"`: a Python `\N{...}` escape that decodes to `bash`
//!   `"/bin/ba*"` / `"/bin/b?sh"`: an sh glob that matches `/bin/bash`
//!   `"{bash,}"`: a brace expansion that gives `bash` (when the sh in use
//!     brace-expands; macOS /bin/sh is bash)
//!   `"ba''sh"`: an sh quote splice that gives `bash`
//!   `"b\\ash"`: Python gives `b\ash`, and sh gives `bash`
//!
//! Asserted: with harmful shell text on stdin (`chmod 777 /etc/hosts`), each
//! command is NOT allow, in both main and subagent mode (deny or ask both
//! pass). A literal whose run-time program cannot be read statically is
//! undetermined, and CLAUDE.md §3 resolves that to the restrictive side.
//!
//! DOES NOT PROVE: which of deny or ask is returned, the reason text,
//! `subprocess` spellings, or that any other glob/brace spelling is covered.
//! "allow" means empty hook stdout.

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
        "bg-ossys-spell-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "ossys-spell-20261116",
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

fn assert_not_allow_both(tag: &str, cmd: &str) {
    let home = unique_home(tag);
    let (m, mr) = run_as(cmd, &home, false);
    let (s, sr) = run_as(cmd, &home, true);
    assert_ne!(m, "allow", "main must not allow; cmd={cmd:?} reason={mr}");
    assert_ne!(
        s, "allow",
        "subagent must not allow; cmd={cmd:?} reason={sr}"
    );
}

#[test]
fn named_unicode_escape_spelling_bash_is_not_allow() {
    assert_not_allow_both(
        "nesc",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("\N{LATIN SMALL LETTER B}ash")'"#,
    );
}

#[test]
fn glob_star_matching_bash_is_not_allow() {
    assert_not_allow_both(
        "star",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("/bin/ba*")'"#,
    );
}

#[test]
fn glob_question_matching_bash_is_not_allow() {
    assert_not_allow_both(
        "qmark",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("/bin/b?sh")'"#,
    );
}

#[test]
fn brace_expansion_spelling_bash_is_not_allow() {
    assert_not_allow_both(
        "brace",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("{bash,}")'"#,
    );
}

#[test]
fn shell_quote_splice_spelling_bash_is_not_allow() {
    assert_not_allow_both(
        "splice",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("ba'"''"'sh")'"#,
    );
}

#[test]
fn shell_backslash_splice_spelling_bash_is_not_allow() {
    assert_not_allow_both(
        "bslash",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("b\\ash")'"#,
    );
}
