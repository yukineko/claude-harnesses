#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog 9778927f: blastguard does not model parenthesised subshells.
//!
//! An unquoted `(` glued to the first word of a segment, or `)` glued to the
//! last word, hides the real command from the judge. Required property: a
//! command wrapped in a subshell gets AT LEAST as strict a verdict as the same
//! command unwrapped (deny stays deny; ask stays ask or deny; never allow).
//!
//! Every command below is only passed as a STRING to the blastguard hook via
//! its PreToolUse JSON on stdin. Nothing is executed.

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

fn unique_home(tag: &str) -> Home {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "bg-9778927f-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(&dir).unwrap();
    Home(dir)
}

/// Run the hook as an INTERACTIVE session (so an Ask stays an Ask);
/// returns (decision, reason).
fn verdict(command: &str) -> (String, String) {
    let home = unique_home("v");
    let payload = serde_json::json!({
        "session_id": "bg-9778927f",
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
    assert!(
        out.status.success(),
        "blastguard exited {:?} for {command:?}; stderr: {}",
        out.status,
        String::from_utf8_lossy(&out.stderr)
    );
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

/// Strictness rank. An unrecognised decision is a test failure, not a rank.
fn rank(decision: &str, command: &str) -> u8 {
    match decision {
        "allow" => 0,
        "ask" => 1,
        "deny" => 2,
        other => panic!("unrecognised decision {other:?} for {command:?}"),
    }
}

/// Assert the bare command has `expected_bare` (control), then that every
/// subshell wrapping is at least as strict. All violations are reported at once.
fn assert_wrappings_not_weaker(bare: &str, expected_bare: &str, wrapped: &[String]) {
    let (bd, br) = verdict(bare);
    assert_eq!(
        bd, expected_bare,
        "control: bare {bare:?} expected {expected_bare}, got {bd} | {br}"
    );
    let base = rank(&bd, bare);
    let mut failures = Vec::new();
    for w in wrapped {
        let (d, r) = verdict(w);
        if rank(&d, w) < base {
            failures.push(format!(
                "{w:?} => {d} | {r:?} (bare {bare:?} => {bd} | {br:?})"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "subshell wrapping weakened the verdict:\n  {}",
        failures.join("\n  ")
    );
}

/// The standard set of subshell wrappings of `cmd`.
fn wrappings(cmd: &str) -> Vec<String> {
    vec![
        format!("({cmd})"),
        format!("( {cmd} )"),
        format!("((({cmd})))"),
        format!("( ({cmd}) )"),
        format!("(true && {cmd})"),
        format!("(echo x; {cmd})"),
        format!("true && ({cmd})"),
        format!("true && (true && {cmd})"),
        format!("({cmd}) && true"),
        format!("({cmd}; echo done)"),
    ]
}

#[test]
fn rm_rf_system_path_in_subshell_is_not_weaker() {
    let cmd = "rm -rf /usr/lib";
    assert_wrappings_not_weaker(cmd, "deny", &wrappings(cmd));
}

#[test]
fn dd_to_raw_disk_in_subshell_is_not_weaker() {
    let cmd = "dd if=/dev/zero of=/dev/sda";
    assert_wrappings_not_weaker(cmd, "deny", &wrappings(cmd));
}

#[test]
fn git_reset_hard_in_subshell_is_not_weaker() {
    let cmd = "git reset --hard";
    assert_wrappings_not_weaker(cmd, "deny", &wrappings(cmd));
}

#[test]
fn git_checkout_force_in_subshell_is_not_weaker() {
    let cmd = "git checkout --force main";
    assert_wrappings_not_weaker(cmd, "deny", &wrappings(cmd));
}

#[test]
fn git_push_force_in_subshell_is_not_weaker() {
    let cmd = "git push --force origin main";
    assert_wrappings_not_weaker(cmd, "ask", &wrappings(cmd));
}

// ------------------------------------------------------- non-regression

#[test]
fn control_benign_subshells_stay_allowed() {
    let mut wrong = Vec::new();
    for cmd in [
        "(echo hi)",
        "( echo hi )",
        "(cd sub && cargo test)",
        "(echo \")\")",
        "(echo '(' ')')",
        "true && (ls -la)",
        "((echo nested))",
    ] {
        let (d, r) = verdict(cmd);
        if d != "allow" {
            wrong.push(format!("{cmd:?} => {d} | {r:?}"));
        }
    }
    assert!(
        wrong.is_empty(),
        "benign subshell no longer allowed:\n  {}",
        wrong.join("\n  ")
    );
}

#[test]
fn control_command_substitution_is_not_misread() {
    // A benign `$(...)` is not a subshell verb and must stay allowed.
    for cmd in ["echo \"$(git rev-parse HEAD)\"", "echo $(pwd)"] {
        let (d, r) = verdict(cmd);
        assert_eq!(d, "allow", "benign command substitution {cmd:?}: {d} | {r}");
    }
}
