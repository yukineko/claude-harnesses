#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Independent tests (written by a test author who did not implement the fix)
//! for the zsh `equals` command-position fail-open, backlog aa7acd12, fixed in
//! blastguard 0.2.694 (commit e8df66146).
//!
//! Claude Code's Bash tool runs /bin/zsh 5.9 with option `equals` on: an
//! unquoted command-position `=word` runs the PATH binary `word` (observed
//! 2026-11-18: `=echo hi` printed `hi`). These tests check that the
//! normalisation reaches the command word behind exec wrappers, inside
//! subshells / groups / chains / command substitutions, and that `=cd` is not
//! tracked as a shell cwd change.
//!
//! The Allow pins in `quoted_or_doubled_equals_forms_stay_allow` were observed
//! with the 0.2.694 worktree debug binary (main and subagent) before being
//! asserted.
//!
//! DOES NOT PROVE: `bash -c '=rm -rf ~'` (spec pending a user decision, not
//! tested here); `=word` in argument position; other zsh-only expansions;
//! that the real zsh agrees with every pinned Allow (`==rm x` was not run in
//! zsh by this author).

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/fixed_tmp.rs"]
mod fixed_tmp;

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
        "bg-zsh-equals-indep-{tag}-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir_all(dir.join("wt")).unwrap();
    Home(dir)
}

fn run_as(command: &str, home: &Home, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "zsh-equals-cmd-independent-20261118",
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

/// Every (command, subagent) pair whose decision fails `ok`.
fn mismatches(cmds: &[&str], tag: &str, want: &str, ok: fn(&str) -> bool) -> Vec<String> {
    let home = unique_home(tag);
    let mut bad = Vec::new();
    for cmd in cmds {
        for subagent in [false, true] {
            let (d, r) = run_as(cmd, &home, subagent);
            if !ok(&d) {
                bad.push(format!(
                    "subagent={subagent} {cmd:?} => {d} (want {want}) ({r})"
                ));
            }
        }
    }
    bad
}

#[test]
fn equals_rm_behind_exec_wrappers_is_deny() {
    let bad = mismatches(
        &[
            "sudo =rm -rf ~",
            "env =rm -rf ~",
            "exec =rm -rf ~",
            "command =rm -rf ~",
            "nohup =rm -rf ~",
            "timeout 5 =rm -rf ~",
        ],
        "wrap",
        "deny",
        |d| d == "deny",
    );
    assert!(
        bad.is_empty(),
        "system harm via wrapper =cmd must be Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn equals_wrapper_itself_is_deny() {
    let bad = mismatches(
        &["=sudo rm -rf ~", "=env rm -rf ~"],
        "eqwrap",
        "deny",
        |d| d == "deny",
    );
    assert!(
        bad.is_empty(),
        "system harm via =wrapper must be Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn equals_rm_in_compound_positions_is_deny() {
    let bad = mismatches(
        &[
            "(=rm -rf ~)",
            "{ =rm -rf ~; }",
            "echo hi && =rm -rf ~",
            "echo $(=rm -rf ~)",
        ],
        "compound",
        "deny",
        |d| d == "deny",
    );
    assert!(
        bad.is_empty(),
        "system harm via =cmd in compound position must be Deny:\n{}",
        bad.join("\n")
    );
}

#[test]
fn equals_chmod_behind_sudo_is_deny() {
    let bad = mismatches(&["sudo =chmod 777 /etc/hosts"], "chmod", "deny", |d| {
        d == "deny"
    });
    assert!(
        bad.is_empty(),
        "system harm via sudo =chmod must be Deny:\n{}",
        bad.join("\n")
    );
}

/// `=$x` with `x=rm` set earlier: Deny if resolved statically, else Ask in
/// main / Deny in subagent. Never Allow.
#[test]
fn equals_variable_command_word_is_not_allow() {
    let home = unique_home("var");
    let cmd = "x=rm; =$x -rf ~";
    let mut bad = Vec::new();
    let (d, r) = run_as(cmd, &home, false);
    if d == "allow" {
        bad.push(format!(
            "subagent=false {cmd:?} => {d} (want not allow) ({r})"
        ));
    }
    let (d, r) = run_as(cmd, &home, true);
    if d != "deny" {
        bad.push(format!("subagent=true {cmd:?} => {d} (want deny) ({r})"));
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `=cd D` runs an external `cd` binary; the shell cwd does not move, so the
/// following relative `rm -rf build` runs in the session cwd, not in D. The
/// control (plain `cd D && rm -rf build`, D a scratch dir under the fixed
/// temp root, same fixture) must be Allow so the difference is attributable
/// to `=cd` alone.
#[test]
fn equals_cd_does_not_move_cwd_into_scratch() {
    let home = unique_home("cd");
    let scratch = fixed_tmp::canonical_fixed_temp_root()
        .join(format!("bg-zsh-equals-indep-cd-{}", std::process::id()));
    std::fs::create_dir_all(scratch.join("build")).unwrap();
    struct Rm(std::path::PathBuf);
    impl Drop for Rm {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    let _guard = Rm(scratch.clone());
    let control = format!("cd {} && rm -rf build", scratch.display());
    let probe = format!("={control}");
    let mut bad = Vec::new();
    for subagent in [false, true] {
        let (d, r) = run_as(&control, &home, subagent);
        if d != "allow" {
            bad.push(format!(
                "CONTROL subagent={subagent} {control:?} => {d} (want allow) ({r})"
            ));
        }
        let (d, r) = run_as(&probe, &home, subagent);
        if d == "allow" {
            bad.push(format!(
                "subagent={subagent} {probe:?} => {d} (want not allow) ({r})"
            ));
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// Quoted / escaped `=` makes a literal command name `=rm` (no such program);
/// `==rm` is stripped only once; `=echo hi` is harmless. Observed Allow with
/// the 0.2.694 debug binary in main and subagent before pinning.
#[test]
fn quoted_or_doubled_equals_forms_stay_allow() {
    let bad = mismatches(
        &["'=rm' x", "\\=rm x", "==rm x", "=echo hi"],
        "allow",
        "allow",
        |d| d == "allow",
    );
    assert!(bad.is_empty(), "pinned Allow changed:\n{}", bad.join("\n"));
}
