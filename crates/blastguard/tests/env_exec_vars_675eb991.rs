#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 675eb991: environment variables whose VALUE is executed as a shell
//! command line (GIT_PAGER, GIT_EDITOR, GIT_SSH_COMMAND, GIT_CONFIG_PARAMETERS,
//! PAGER, EDITOR) must be judged, not skipped as a leading `NAME=value`.
//!
//! These tests only pass command STRINGS to the blastguard hook on stdin; no
//! command under test is ever executed.

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

fn unique_home() -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-envexec-675eb991-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Run the hook as an INTERACTIVE session (an Ask stays an Ask); returns
/// (decision, reason).
fn verdict(command: &str) -> (String, String) {
    let home = unique_home();
    let payload = serde_json::json!({
        "session_id": "envexec-675eb991",
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

#[derive(Clone, Copy)]
enum Expect {
    Deny,
    NotAllow,
    Allow,
}

/// Judge every case and panic once with ALL violations listed.
fn check_all(cases: &[(&str, Expect)]) {
    let mut violations = Vec::new();
    for (cmd, expect) in cases {
        let (d, r) = verdict(cmd);
        let ok = match expect {
            Expect::Deny => d == "deny",
            Expect::NotAllow => d != "allow",
            Expect::Allow => d == "allow",
        };
        if !ok {
            let want = match expect {
                Expect::Deny => "deny",
                Expect::NotAllow => "not allow (ask|deny)",
                Expect::Allow => "allow",
            };
            violations.push(format!(
                "  `{cmd}`: expected {want}, got decision={d} reason={r:?}"
            ));
        }
    }
    assert!(
        violations.is_empty(),
        "{} violation(s):\n{}",
        violations.len(),
        violations.join("\n")
    );
}

/// Sanity: the payload itself and the `git -c` twin are already judged.
#[test]
fn control_payload_and_git_c_twin_are_denied() {
    check_all(&[
        ("rm -rf /usr/lib", Expect::Deny),
        ("git -c core.pager='rm -rf /usr/lib' log", Expect::Deny),
    ]);
}

/// Leading-assignment form with a literal destructive payload: must deny.
#[test]
fn leading_assignment_exec_var_with_destructive_payload_is_denied() {
    check_all(&[
        ("GIT_PAGER='rm -rf /usr/lib' git log", Expect::Deny),
        ("GIT_SSH_COMMAND='rm -rf /usr/lib' git fetch", Expect::Deny),
        ("GIT_EDITOR='rm -rf /usr/lib' git commit", Expect::Deny),
        ("PAGER='rm -rf /usr/lib' man ls", Expect::Deny),
        ("EDITOR='rm -rf /usr/lib' crontab -e", Expect::Deny),
    ]);
}

/// `env NAME=...`, `export NAME=...;` and GIT_CONFIG_PARAMETERS forms: must not
/// be allowed.
#[test]
fn env_export_and_config_parameters_forms_are_not_allowed() {
    check_all(&[
        (
            "GIT_CONFIG_PARAMETERS=\"'core.pager=rm -rf /usr/lib'\" git log",
            Expect::NotAllow,
        ),
        (
            "env GIT_SSH_COMMAND='rm -rf /usr/lib' git fetch",
            Expect::NotAllow,
        ),
        (
            "export GIT_SSH_COMMAND='rm -rf /usr/lib'; git fetch",
            Expect::NotAllow,
        ),
    ]);
}

/// A value only known at run time cannot be judged: must not be allowed.
#[test]
fn runtime_only_exec_var_value_is_not_allowed() {
    check_all(&[("GIT_SSH_COMMAND=\"$X\" git fetch", Expect::NotAllow)]);
}

/// Non-regression: benign values and unrelated variables stay allowed.
#[test]
fn benign_exec_var_values_stay_allowed() {
    check_all(&[
        ("GIT_PAGER=cat git log", Expect::Allow),
        ("EDITOR=vim git commit", Expect::Allow),
        (
            "GIT_SSH_COMMAND='ssh -i ~/.ssh/key' git fetch",
            Expect::Allow,
        ),
        ("PAGER=less man ls", Expect::Allow),
        ("FOO=bar ls", Expect::Allow),
        ("LANG=C git status", Expect::Allow),
        (
            "GIT_CONFIG_PARAMETERS=\"'core.pager=cat'\" git log",
            Expect::Allow,
        ),
    ]);
}
