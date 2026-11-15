#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Consistency for quoted numeric dup targets (backlog cf0bf4ca, follow-up of
//! the verifier's fail on be11dadba, 2026-11-15): bash removes the quotes of a
//! `<&` / `>&` target before it applies the redirection, so `<&'3'` and
//! `<&"3"` are `<&3`. Every case here asserts that the quoted spelling gets
//! exactly the (main, subagent) verdict pair of the unquoted spelling, and the
//! harmful shell/eval readers additionally deny/deny in both modes.
//!
//! Observed on be11dadba (before this fix): the eval / `$(…)` cases with one
//! quoted `2>&'3'` dropped to ask/deny (unquoted deny/deny), and `cat <&'3'`,
//! `bash <&'3'` on benign text, `echo x >&'3'` differed from their unquoted
//! forms, because the raw-text mention count read only unquoted digits.
//!
//! DOES NOT PROVE: fd numbers other than 3, harm other than `chmod 777
//! /etc/hosts`, mixed quoting such as `<&3''`, or the reason text.
//!
//! Written by the implementer of the fix (the caller allowed it); the
//! independent verifier probes separately.

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
        "bg-qdupc-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "quoted-dup-consistency-20261115",
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
        return "allow".into();
    }
    let v: serde_json::Value = serde_json::from_str(stdout.trim()).unwrap();
    v["hookSpecificOutput"]["permissionDecision"]
        .as_str()
        .unwrap_or("?")
        .to_string()
}

const HARM: &str = "exec 3<<<'chmod 777 /etc/hosts'; ";
const BENIGN: &str = "exec 3<<<'echo hi'; ";

fn verdicts(cmd: &str, tag: &str) -> (String, String) {
    let home = unique_home(tag);
    (run_as(cmd, &home, false), run_as(cmd, &home, true))
}

/// `template` holds `@` where the dup target goes; the quoted spellings of
/// `3` must give the verdict pair of the unquoted `3`.
fn assert_same_as_unquoted(prefix: &str, template: &str, tag: &str) -> (String, String) {
    let plain = format!("{prefix}{}", template.replace('@', "3"));
    let want = verdicts(&plain, &format!("{tag}-u"));
    for (q, quoted) in [("sq", "'3'"), ("dq", "\"3\"")] {
        let cmd = format!("{prefix}{}", template.replace('@', quoted));
        let got = verdicts(&cmd, &format!("{tag}-{q}"));
        assert_eq!(
            got, want,
            "quoted {cmd:?} must match unquoted {plain:?} (main, subagent)"
        );
    }
    want
}

fn assert_deny_deny(v: (String, String), what: &str) {
    assert_eq!(
        v,
        ("deny".to_string(), "deny".to_string()),
        "{what} must be deny/deny"
    );
}

#[test]
fn eval_reader_with_quoted_stderr_dup_stays_deny() {
    let v = assert_same_as_unquoted(HARM, "eval 'bash <&3' 2>&@", "eval");
    assert_deny_deny(v, "eval 'bash <&3' 2>&N");
}

#[test]
fn echo_cmdsub_reader_with_quoted_stderr_dup_stays_deny() {
    let v = assert_same_as_unquoted(HARM, "echo \"$(bash <&3)\" 2>&@", "echo");
    assert_deny_deny(v, "echo \"$(bash <&3)\" 2>&N");
}

#[test]
fn assign_cmdsub_reader_with_quoted_stderr_dup_stays_deny() {
    let v = assert_same_as_unquoted(HARM, "x=$(bash <&3) 2>&@", "assign");
    assert_deny_deny(v, "x=$(bash <&3) 2>&N");
}

#[test]
fn cat_reads_quoted_dup_like_unquoted_harm_text() {
    assert_same_as_unquoted(HARM, "cat <&@", "catharm");
}

#[test]
fn cat_reads_quoted_dup_like_unquoted_benign_text() {
    assert_same_as_unquoted(BENIGN, "cat <&@", "catok");
}

#[test]
fn bash_reads_quoted_dup_like_unquoted_benign_text() {
    assert_same_as_unquoted(BENIGN, "bash <&@", "bashok");
}

#[test]
fn echo_writes_quoted_dup_like_unquoted() {
    assert_same_as_unquoted(HARM, "echo x >&@", "echox");
}

#[test]
fn bash_reads_quoted_move_dup_like_unquoted_and_denies() {
    let v = assert_same_as_unquoted(HARM, "bash <&@-", "move");
    assert_deny_deny(v, "bash <&N-");
}
