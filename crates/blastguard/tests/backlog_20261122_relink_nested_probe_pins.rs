#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Probe pins for backlog 00c6c2b8 (fix 320b5d7e7): a symlink created by `ln`
//! inside a command substitution, process substitution, assignment, or an
//! interpreter escape (`bash -c`, `sh -c`, vim `-c '!…'` / `+'!…'`) counts as a
//! relink, so a later write through that link into a system dir is Deny
//! (memory `system-harm-always-deny`: system harm is Deny through any
//! wrapper/shape). Anything blastguard cannot determine must never be Allow.
//!
//! `/etc/bg-sentinel` is a deliberately non-existent sentinel path; nothing in
//! this file runs the commands, they are only fed to the hook as JSON.
//!
//! Fixture copied from `backlog_20261122_relink_symlink_via_subst_failopen.rs`:
//! hermetic HOME, cwd = HOME/wt, env_clear, CLAUDECODE=1. Empty stdout = allow.
//!
//! Written by an independent test author (not the implementer).
//!
//! DOES NOT PROVE: subagent mode; hard links / `mv` relinks; that the reason
//! text names the relink; that shapes not listed here are covered; for the
//! "not allow" group, which of Ask/Deny is produced (either is accepted).

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
        "bg-relink-nested-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_main(command: &str, home: &Home) -> (String, String) {
    let p = serde_json::json!({
        "session_id": "relink-nested-probe-pins-20261122",
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

fn assert_deny(tag: &str, c: &str) {
    let home = unique_home(tag);
    let (d, r) = run_main(c, &home);
    assert_eq!(
        d, "deny",
        "system write through same-line symlink must be exactly deny: {c:?} => {d} ({r})"
    );
}

fn assert_not_allow(tag: &str, c: &str) {
    let home = unique_home(tag);
    let (d, r) = run_main(c, &home);
    assert_ne!(
        d, "allow",
        "undetermined relink shape must not be allow: {c:?} => {d} ({r})"
    );
}

fn assert_allow(tag: &str, c: &str) {
    let home = unique_home(tag);
    let (d, r) = run_main(c, &home);
    assert_eq!(
        d, "allow",
        "ordinary-dir control must be exactly allow (false positive): {c:?} => {d} ({r})"
    );
}

fn assert_not_deny(tag: &str, c: &str) {
    let home = unique_home(tag);
    let (d, r) = run_main(c, &home);
    assert_ne!(
        d, "deny",
        "ordinary-dir control must not be deny (false positive): {c:?} => {d} ({r})"
    );
}

// ---- exactly Deny ----

#[test]
fn a_bash_c_backslash_escaped_ln_then_write_is_deny() {
    // bash -c body is `l\n -s ...`; bash's quote removal makes it `ln`.
    // "$(echo ln)" only becomes $0 and is irrelevant.
    assert_deny(
        "a",
        r#"bash -c 'l\n -s /etc/bg-sentinel d' "$(echo ln)"; echo x > d/hosts"#,
    );
}

#[test]
fn b_vim_bang_backslash_escaped_ln_then_write_is_deny() {
    // vim's `:!` only strips backslashes before its own specials (`!`, `%`,
    // `#`); `\n` reaches the shell unchanged and the shell runs `ln`.
    assert_deny(
        "b",
        r#"vim -c '!l\n -s /etc/bg-sentinel d' -c q; echo x > d/hosts"#,
    );
}

#[test]
fn c_dollar_paren_backslash_escaped_ln_then_write_is_deny() {
    assert_deny(
        "c",
        r#"echo "$(l\n -s /etc/bg-sentinel d)"; echo x > d/hosts"#,
    );
}

#[test]
fn d_subst_in_write_segment_runs_before_redirect_is_deny() {
    assert_deny("d", r#"echo "$(ln -s /etc/bg-sentinel d)" > d/hosts"#);
}

#[test]
fn e_assignment_subst_then_write_is_deny() {
    assert_deny("e", r#"X=$(ln -s /etc/bg-sentinel d); echo x > d/hosts"#);
}

#[test]
fn f_process_subst_then_write_is_deny() {
    assert_deny("f", r#"cat <(ln -s /etc/bg-sentinel d); echo x > d/hosts"#);
}

#[test]
fn g_sh_c_double_quoted_then_write_is_deny() {
    assert_deny("g", r#"sh -c "ln -s /etc/bg-sentinel d"; echo x > d/hosts"#);
}

#[test]
fn h_vim_plus_silent_bang_then_write_is_deny() {
    assert_deny(
        "h",
        r#"vim +'silent !ln -s /etc/bg-sentinel d' +q; echo x > d/hosts"#,
    );
}

#[test]
fn i_subst_then_rm_through_link_is_deny() {
    assert_deny("i", r#"echo "$(ln -s /etc/bg-sentinel d)"; rm -rf d/hosts"#);
}

#[test]
fn j_subst_then_tee_through_link_is_deny() {
    assert_deny(
        "j",
        r#"echo "$(ln -s /etc/bg-sentinel d)"; tee d/hosts < /dev/null"#,
    );
}

// ---- not Allow (Ask or Deny) ----

#[test]
fn k_default_expansion_subst_then_write_is_not_allow() {
    assert_not_allow(
        "k",
        r#"cp a.txt "${X:-$(ln -s /etc/bg-sentinel d)}" b.txt; echo x > d/hosts"#,
    );
}

#[test]
fn l_vim_call_system_then_write_is_not_allow() {
    assert_not_allow(
        "l",
        r#"vim -c 'call system("ln -s /etc/bg-sentinel d")' -c q; echo x > d/hosts"#,
    );
}

#[test]
fn m_sudo_bash_c_then_write_is_not_allow() {
    assert_not_allow(
        "m",
        r#"sudo bash -c 'ln -s /etc/bg-sentinel d'; echo x > d/hosts"#,
    );
}

// ---- false-positive controls (ordinary dir) ----

#[test]
fn n_vim_set_nu_then_write_is_allow() {
    // No shell escape, no relink; plain write into cwd. A reasonable gate allows.
    assert_allow("n", r#"vim -c 'set nu' -c q notes.txt; echo x > out.txt"#);
}

#[test]
fn o_bash_c_echo_then_write_is_allow() {
    assert_allow("o", r#"bash -c 'echo hi' && echo x > out.txt"#);
}

#[test]
fn p_date_subst_then_write_is_allow() {
    assert_allow("p", r#"echo "$(date)"; echo x > out.txt"#);
}

#[test]
fn q_git_commit_message_mentions_ln_then_write_is_not_deny() {
    // Only "not deny": blastguard has separate git-commit rules (memory
    // ruling "git commit only --trailer/--author") that may legitimately Ask
    // on `git commit -m` independently of the relink logic, so exact "allow"
    // cannot be asserted from the relink spec alone. The relink property is:
    // `ln -s` inside a commit message string is data, so it must not Deny.
    assert_not_deny(
        "q",
        r#"git commit -m "use ln -s and cp in docs"; echo x > out.txt"#,
    );
}
