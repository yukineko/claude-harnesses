#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Observation tests for backlog 68b1a9dc: `/bin/echo` and `/usr/bin/echo`
//! as literal pipe producers, plus the `echo ... > /dev/stdout | sh` Deny
//! reading. Written by an independent test author, not the implementer.
//!
//! Harness copied from backlog_68b1a9dc_controls.rs.
//!
//! DOES NOT PROVE: any shape not listed; any non-Bash tool; that a harmful body
//! delivered by some other producer ends at Deny; that "not allow" cases are
//! Deny (Ask is accepted); that "not deny" cases are Allow.

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
        "bg-68b1a9dc-abs-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn decide(command: &str, home: &Home, subagent: bool) -> String {
    let mut p = serde_json::json!({
        "session_id": "backlog-68b1a9dc-abs-echo-20261121",
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

/// System harm: Deny in both modes.
fn check_deny_both(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let main = decide(cmd, &home, false);
    let sub = decide(cmd, &home, true);
    eprintln!("[{tag}] {cmd:?} main={main} subagent={sub}");
    assert!(
        main == "deny" && sub == "deny",
        "expected deny in both modes for {cmd:?}; observed main={main} subagent={sub}"
    );
}

/// Neither mode may be Allow.
fn check_not_allow_both(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let main = decide(cmd, &home, false);
    let sub = decide(cmd, &home, true);
    eprintln!("[{tag}] {cmd:?} main={main} subagent={sub}");
    let ok = |d: &str| d == "ask" || d == "deny";
    assert!(
        ok(&main) && ok(&sub),
        "expected not-allow in both modes for {cmd:?}; observed main={main} subagent={sub}"
    );
}

/// FP control: main mode must not be Deny.
fn check_not_deny_main(cmd: &str, tag: &str) {
    let home = unique_home(tag);
    let main = decide(cmd, &home, false);
    let sub = decide(cmd, &home, true);
    eprintln!("[{tag}] {cmd:?} main={main} subagent={sub}");
    assert!(
        main != "deny",
        "expected main != deny for {cmd:?}; observed main={main} subagent={sub}"
    );
}

/// Does NOT prove: other xargs consumers or other targets than /etc.
#[test]
fn a1_bin_echo_xargs_rm_deny() {
    check_deny_both("/bin/echo /etc | xargs rm -rf", "a1");
}

/// Does NOT prove: /bin/echo variant (a1) or other absolute-path producers.
#[test]
fn a2_usr_bin_echo_xargs_rm_deny() {
    check_deny_both("/usr/bin/echo /etc | xargs rm -rf", "a2");
}

/// Does NOT prove: other interpreters or other python payload spellings.
#[test]
fn a3_bin_echo_python_rmtree_deny() {
    check_deny_both(
        "/bin/echo 'import shutil; shutil.rmtree(\"/etc\")' | python3",
        "a3",
    );
}

/// Does NOT prove: other harmful bodies or targets besides /etc.
#[test]
fn a4_bin_echo_sh_rm_deny() {
    check_deny_both("/bin/echo rm -rf /etc | sh", "a4");
}

/// Does NOT prove: `bash` without `-s`, or other flags.
#[test]
fn a5_bin_echo_bash_s_rm_usr_deny() {
    check_deny_both("/bin/echo rm -rf /usr | bash -s", "a5");
}

/// Does NOT prove: `>` or other fds (only `>> /dev/fd/1` is exercised).
#[test]
fn a6_echo_append_dev_fd1_bash_deny() {
    check_deny_both("echo rm -rf /etc >> /dev/fd/1 | bash", "a6");
}

/// Does NOT prove: deeper nesting or nested bodies with other redirects.
#[test]
fn a7_nested_echo_dev_stdout_bash_deny() {
    check_deny_both("echo 'echo rm -rf /etc > /dev/stdout | bash' | sh", "a7");
}

/// Does NOT prove: that the function is ever really shadowing; only not-Allow.
#[test]
fn b1_shadowed_bin_echo_function_not_allow() {
    check_not_allow_both("/bin/echo() { :; }; /bin/echo rm -rf /etc | bash", "b1");
}

/// Does NOT prove: that the inner shadow is detected; only not-Allow.
#[test]
fn b2_shadowed_bin_echo_in_bash_c_not_allow() {
    check_not_allow_both(
        "bash -c '/bin/echo() { :; }; /bin/echo rm -rf /etc | bash'",
        "b2",
    );
}

/// Does NOT prove the leading-dash ruling (sibling item dd5c4fe9 awaits a
/// human ruling); only not-Allow.
#[test]
fn b3_bin_echo_leading_dash_not_allow() {
    check_not_allow_both("/bin/echo -n rm -rf /etc | bash", "b3");
}

/// Does NOT prove: other backslash spellings; only not-Allow.
#[test]
fn b4_bin_echo_backslash_not_allow() {
    check_not_allow_both("/bin/echo rm\\ -rf\\ /etc | bash", "b4");
}

/// Does NOT prove: /sbin/echo is treated as echo; only not-Allow.
#[test]
fn b5_sbin_echo_not_allow() {
    check_not_allow_both("/sbin/echo rm -rf /etc | bash", "b5");
}

/// Does NOT prove: Allow (Ask accepted), nor subagent-mode behaviour.
#[test]
fn f1_bin_echo_ls_bash_not_deny() {
    check_not_deny_main("/bin/echo ls | bash", "f1");
}

/// Does NOT prove: Allow (Ask accepted), nor subagent-mode behaviour.
#[test]
fn f2_bin_echo_hello_xargs_echo_not_deny() {
    check_not_deny_main("/bin/echo hello | xargs echo", "f2");
}

/// Does NOT prove: Allow (Ask accepted), nor subagent-mode behaviour.
#[test]
fn f3_echo_ls_dev_stdout_bash_not_deny() {
    check_not_deny_main("echo ls > /dev/stdout | bash", "f3");
}
