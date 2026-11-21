#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Regression CONTROLS for backlog 68b1a9dc (echo-feeder variants of
//! `rm -rf /etc` piped to a shell must become Deny). These guard the fix against
//! (a) false positives (benign / non-delivering shapes turning Deny) and
//! (b) new fail-opens (shapes that must stay at least Ask turning Allow).
//! Written by an independent test author, not the implementer.
//!
//! FP controls assert main mode only: subagent mode hardens Ask to Deny, so
//! "not Deny" cannot hold there. Fail-open controls assert both modes.
//!
//! DOES NOT PROVE: that the pinned REDs in
//! backlog_20261120_8cadc78f_adversarial.rs pass; shapes not listed here; any
//! non-Bash tool; that a non-Deny FP control is Allow (Ask is accepted).

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
        "bg-68b1a9dc-ctl-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "backlog-68b1a9dc-controls-20261121",
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

/// FP control: main-mode decision must not be Deny.
fn check_not_deny_main(cmd: &str, tag: &str, property: &str) {
    let home = unique_home(tag);
    let main = decide(cmd, &home, false);
    let sub = decide(cmd, &home, true);
    eprintln!("[{tag}] {cmd:?} main={main} subagent={sub}");
    assert!(
        main != "deny",
        "property [{property}] expected main != deny for {cmd:?}; observed main={main} subagent={sub}"
    );
}

/// Fail-open control: neither mode may be Allow.
fn check_not_allow_both(cmd: &str, tag: &str, property: &str) {
    let home = unique_home(tag);
    let main = decide(cmd, &home, false);
    let sub = decide(cmd, &home, true);
    eprintln!("[{tag}] {cmd:?} main={main} subagent={sub}");
    let bad = |d: &str| !(d == "ask" || d == "deny");
    assert!(
        !bad(&main) && !bad(&sub),
        "property [{property}] expected not-allow in both modes for {cmd:?}; observed main={main} subagent={sub}"
    );
}

// ---- FP side (main mode != deny) ----

/// Property: a benign literal program through `env bash` is not Deny.
/// Does NOT prove it is Allow.
#[test]
fn c01_echo_ls_env_bash_not_deny() {
    check_not_deny_main("echo ls | env bash", "c01", "benign echo to env bash");
}

/// Property: a benign nested echo|env bash inside a literal body is not Deny.
/// Does NOT prove the inner command is ever judged.
#[test]
fn c02_nested_benign_body_not_deny() {
    check_not_deny_main(
        "echo 'echo ls | env bash' | bash",
        "c02",
        "benign nested literal body",
    );
}

/// Property: when echo's stdout goes to a file the shell gets an empty program,
/// so the harmful words are not delivered and it is not Deny.
/// Does NOT prove it is Allow (an unrelated file write may Ask).
#[test]
fn c03_echo_to_file_then_pipe_not_deny() {
    check_not_deny_main(
        "echo rm -rf /etc > out68.txt | bash",
        "c03",
        "stdout redirected to file",
    );
}

/// Property: echo to /dev/stderr delivers nothing to the pipe, so not Deny.
/// Does NOT prove it is Allow.
#[test]
fn c04_echo_to_stderr_then_pipe_not_deny() {
    check_not_deny_main(
        "echo rm -rf /etc > /dev/stderr | bash",
        "c04",
        "stdout to /dev/stderr",
    );
}

/// Property: a benign body with a variable operand is not Deny.
/// Does NOT prove variable expansion is resolved.
#[test]
fn c05_benign_var_operand_not_deny() {
    check_not_deny_main(
        "X=/tmp/x68; echo ls $X | bash",
        "c05",
        "benign variable operand",
    );
}

/// Property: `echo hi > /dev/stdout | bash` (benign) is not Deny.
/// Does NOT prove it is Allow.
#[test]
fn c06_benign_dev_stdout_not_deny() {
    check_not_deny_main(
        "echo hi > /dev/stdout | bash",
        "c06",
        "benign /dev/stdout producer",
    );
}

/// Property: `/bin/echo ls | bash` (benign) is not Deny.
/// Does NOT prove it is Allow.
#[test]
fn c07_benign_bin_echo_not_deny() {
    check_not_deny_main("/bin/echo ls | bash", "c07", "benign /bin/echo producer");
}

/// Property: backslash-escaped space operand with a benign program is not Deny.
/// Does NOT prove the escape is decoded.
#[test]
fn c08_benign_escaped_space_not_deny() {
    check_not_deny_main(
        "echo ls\\ -la | bash",
        "c08",
        "benign escaped-space operand",
    );
}

// ---- fail-open side (both modes != allow) ----

/// Property: a non-system `echo` path is not treated as safe; never Allow.
/// Does NOT prove Deny.
#[test]
fn c09_tmp_echo_not_allow() {
    check_not_allow_both(
        "/tmp/echo rm -rf /etc | bash",
        "c09",
        "/tmp/echo not system",
    );
}

/// Property: `./echo` is not treated as safe; never Allow.
/// Does NOT prove Deny.
#[test]
fn c10_dot_slash_echo_not_allow() {
    check_not_allow_both("./echo rm -rf /etc | bash", "c10", "./echo not system");
}

/// Property: `2> /dev/stdout` (fd 2) must not be mistaken for a benign/empty
/// feeder; never Allow. Does NOT prove Deny.
#[test]
fn c11_stderr_to_dev_stdout_not_allow() {
    check_not_allow_both(
        "echo rm -rf /etc 2> /dev/stdout | bash",
        "c11",
        "fd2 to /dev/stdout",
    );
}

/// Property: stdout to /dev/stdout plus a later fd-2 redirect to a file never
/// Allow. Does NOT prove Deny.
#[test]
fn c12_stdout_then_stderr_file_not_allow() {
    check_not_allow_both(
        "echo rm -rf /etc > /dev/stdout 2> f68 | bash",
        "c12",
        "stdout to /dev/stdout with extra fd2 redirect",
    );
}

/// Property: `env -i bash` consumer does not hide the body; never Allow.
/// Does NOT prove Deny.
#[test]
fn c13_env_i_bash_not_allow() {
    check_not_allow_both("echo rm -rf /etc | env -i bash", "c13", "env -i consumer");
}

/// Property: a leading-dash `/bin/echo -n` feeder is never Allow. Deliberately
/// asserts ONLY not-allow: sibling item dd5c4fe9 (leading-dash echo operands)
/// awaits a human ruling. Does NOT pin Deny or any stronger decision.
#[test]
fn c14_bin_echo_dash_n_not_allow() {
    check_not_allow_both(
        "/bin/echo -n rm -rf /etc | bash",
        "c14",
        "leading-dash echo operand, not-allow only",
    );
}

/// Property: variable-held target plus /dev/stdout plus env bash is never Allow.
/// Does NOT prove Deny.
#[test]
fn c15_var_target_dev_stdout_env_bash_not_allow() {
    check_not_allow_both(
        "Y=/etc; echo rm -rf $Y > /dev/stdout | env bash",
        "c15",
        "variable target via /dev/stdout to env bash",
    );
}
