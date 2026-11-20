#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! RED (backlog a761b38f): sudo(8) long-form options are not modelled in
//! `unwrap_at` (Table B.5), so `sudo --user root ls -la` is Ask.
//!
//! Observed 2026-11-20: `detect::tests::d3_benign_long_form_wrapper_invocations_stay_allowed`
//! fails with Ask("`sudo` option `--user` is not modelled ...").
//!
//! Intended behaviour pinned here:
//!   * benign long-form invocations are Allow (same as the short-form equivalents);
//!   * dangerous commands behind long-form options stay Deny;
//!   * UNMODELLED options stay restrictive (not Allow);
//!   * a long option that takes a value consumes it (`--user rm -rf /etc` is not Allow);
//!   * `doas` has no long options, so `doas --user root ls` is not Allow.
//!
//! Written by an independent test author.

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
        "bg-sudolong-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

/// Run the hook. Returns (decision, reason).
fn run_as(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "sudo-long-form-probe-20261121",
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

fn decide(tag: &str, command: &str) -> (String, String) {
    let home = unique_home(tag);
    run_as(command, &home)
}

fn assert_allow(tag: &str, command: &str) {
    let (d, r) = decide(tag, command);
    assert_eq!(d, "allow", "{command:?} must be allow, got decision={d} reason={r}");
}

fn assert_deny(tag: &str, command: &str) {
    let (d, r) = decide(tag, command);
    assert_eq!(d, "deny", "{command:?} must be deny, got decision={d} reason={r}");
}

fn assert_not_allow(tag: &str, command: &str) {
    let (d, r) = decide(tag, command);
    assert_ne!(
        d, "allow",
        "{command:?} must be ask/deny, got decision={d} reason={r}"
    );
}

/// The long form must get the same decision as the short-form equivalent.
fn assert_same_as(tag: &str, long: &str, short: &str) {
    let (dl, rl) = decide(&format!("{tag}l"), long);
    let (ds, rs) = decide(&format!("{tag}s"), short);
    eprintln!("OBSERVED short {short:?} => {ds} ({rs})");
    eprintln!("OBSERVED long  {long:?} => {dl} ({rl})");
    assert_eq!(
        dl, ds,
        "{long:?} => {dl} ({rl}) but short form {short:?} => {ds} ({rs})"
    );
}

// ---- a) benign long forms are Allow ----

#[test]
fn a_user_space() {
    assert_allow("a1", "sudo --user root ls -la");
}

#[test]
fn a_user_equals() {
    assert_allow("a2", "sudo --user=root ls -la");
}

#[test]
fn a_group() {
    assert_allow("a3", "sudo --group wheel ls");
}

#[test]
fn a_chdir() {
    assert_allow("a4", "sudo --chdir /tmp ls");
}

#[test]
fn a_non_interactive() {
    assert_allow("a5", "sudo --non-interactive ls");
}

#[test]
fn a_preserve_env() {
    assert_allow("a6", "sudo --preserve-env ls");
}

#[test]
fn a_set_home_user_cargo() {
    assert_allow("a7", "sudo --set-home --user root cargo build");
}

#[test]
fn a_other_value_options() {
    assert_allow("a8", "sudo --close-from 5 ls");
    assert_allow("a9", "sudo --host myhost ls");
    assert_allow("a10", "sudo --prompt=pw: ls");
    assert_allow("a11", "sudo --role r ls");
    assert_allow("a12", "sudo --type t ls");
    assert_allow("a13", "sudo --other-user bob ls");
}

#[test]
fn a_flag_options() {
    assert_allow("a14", "sudo --stdin ls");
    assert_allow("a15", "sudo --reset-timestamp ls");
    assert_allow("a16", "sudo --askpass ls");
    assert_allow("a17", "sudo --preserve-groups ls");
    assert_allow("a18", "sudo --background ls");
}

#[test]
fn a_doas_has_no_long_options() {
    assert_not_allow("a19", "doas --user root ls");
}

// ---- b) dangerous through long forms ----

#[test]
fn b_user_bash_c_rm_etc_denied() {
    assert_deny("b1", "sudo --user root bash -c 'rm -rf /etc'");
}

#[test]
fn b_user_equals_rm_etc_denied() {
    assert_deny("b2", "sudo --user=root rm -rf /etc");
}

#[test]
fn b_login_herestring_matches_short() {
    assert_same_as("b3", "sudo --login <<< 'rm -rf /etc'", "sudo -i <<< 'rm -rf /etc'");
    assert_not_allow("b3n", "sudo --login <<< 'rm -rf /etc'");
}

#[test]
fn b_shell_herestring_matches_short() {
    assert_same_as("b4", "sudo --shell <<< 'rm -rf /etc'", "sudo -s <<< 'rm -rf /etc'");
    assert_not_allow("b4n", "sudo --shell <<< 'rm -rf /etc'");
}

// ---- c) e1bb5f7e controls ----

#[test]
fn c_sudo_bash_stdin_not_allow() {
    assert_not_allow("c1", "sudo bash < /tmp/p");
}

#[test]
fn c_sudo_bash_c_rm_etc_denied() {
    assert_deny("c2", "sudo bash -c 'rm -rf /etc'");
}

#[test]
fn c_long_user_bash_stdin_not_allow() {
    assert_not_allow("c3", "sudo --user root bash < /tmp/p");
}

// ---- d) unmodelled long options stay restrictive ----

#[test]
fn d_unknown_option_not_allow() {
    assert_not_allow("d1", "sudo --frobnicate ls");
}

#[test]
fn d_edit_not_allow() {
    assert_not_allow("d2", "sudo --edit /etc/passwd");
}

#[test]
fn d_list_rm_not_allow() {
    assert_not_allow("d3", "sudo --list rm");
}

#[test]
fn d_version_matches_short() {
    assert_same_as("d4", "sudo --version", "sudo -V");
}

// ---- e) value-taking long option consumes its value ----

#[test]
fn e_user_consumes_rm_then_unknown_option() {
    // `--user rm` takes `rm` as the user NAME; `-rf` is then an unknown sudo option.
    assert_not_allow("e1", "sudo --user rm -rf /etc");
}
