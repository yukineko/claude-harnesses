#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (3da14bbd follow-up round 2, written 2026-11-16 by the coordinator, who
//! did not write the fix; suspected by the independent verifier of efb6116f4):
//!
//! The literal `os.system("...")` command is judged by its words. The shell
//! run by os.system can read the inherited stdin as sh code without any
//! listed shell/interpreter word: the `.` / `source` builtin on `/dev/stdin`,
//! or `eval "$(cat)"`. (sh is run by os.system itself, so the harmful stdin
//! text runs as sh code.)
//!
//! Asserted: with harmful shell text on stdin (`chmod 777 /etc/hosts`), each
//! command is NOT allow, in both main and subagent mode (deny or ask both
//! pass).
//!
//! DOES NOT PROVE: which of deny or ask is returned, the reason text,
//! `/dev/fd/0` or `/proc/self/fd/0` spellings, or other builtins.
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
        "bg-ossys-src-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "ossys-src-20261116",
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
fn dot_source_dev_stdin_is_not_allow() {
    assert_not_allow_both(
        "dot",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system(". /dev/stdin")'"#,
    );
}

#[test]
fn source_dev_stdin_is_not_allow() {
    assert_not_allow_both(
        "source",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("source /dev/stdin")'"#,
    );
}

#[test]
fn eval_cat_stdin_is_not_allow() {
    assert_not_allow_both(
        "evalcat",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("eval \"$(cat)\"")'"#,
    );
}

#[test]
fn xargs_sh_c_is_not_allow() {
    assert_not_allow_both(
        "xargs",
        r#"echo 'chmod 777 /etc/hosts' | python3 -c 'import os; os.system("xargs -0 sh -c")'"#,
    );
}
