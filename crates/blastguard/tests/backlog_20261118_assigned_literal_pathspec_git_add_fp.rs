#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]
//! Backlog (2026-11-18): resolution gap observed live (relay
//! bgr-4cf239fd7389). `a=<literal path> && git add -- $a` binds `a` to a
//! literal on the same line, so `$a` is text-determined (standing ruling
//! "blastguard must resolve statically"); blastguard still answers
//! "cannot determine whether this git add/commit is whole-tree staging (an
//! unquoted expansion, glob or brace list the shell may split)".
//!
//! Measured 2026-11-18: a single `a=lit && git add -- $a` IS already Allow;
//! the gap shows with TWO assigned variables (`$a $b`) and with a literal
//! `cd` before the assignment (the live command had both).
//!
//! REDs: those two forms are Allow in both modes, like the literal twin.
//! Controls: the one-variable form and the literal twin are Allow; an
//! UNBOUND `$a` stays not Allow.
//!
//! DOES NOT PROVE: `git commit -- $a`, several variables, `;` joins, or a
//! literal containing spaces / globs.

#[path = "support/bounded.rs"]
mod bounded;
#[path = "support/git_env.rs"]
mod git_env;
#[path = "support/neutral_base.rs"]
mod neutral_base;

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

struct Fx {
    base: PathBuf,
}

impl Drop for Fx {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.base);
    }
}

fn git(dir: &Path, args: &[&str]) {
    let mut cmd = git_env::pinned(dir);
    cmd.args([
        "-c",
        "user.name=t",
        "-c",
        "user.email=t@example.invalid",
        "-c",
        "core.hooksPath=/dev/null",
        "-c",
        "init.defaultBranch=main",
    ])
    .args(args)
    .env("GIT_CONFIG_GLOBAL", "/dev/null")
    .env("GIT_CONFIG_NOSYSTEM", "1");
    let out =
        bounded::run_bounded_stdin(&mut cmd, bounded::ChildStdin::Null, bounded::TOOL_DEADLINE);
    assert!(out.status.success(), "git {args:?} failed");
}

fn repo(tag: &str) -> (Fx, PathBuf) {
    use std::sync::atomic::{AtomicUsize, Ordering};
    static N: AtomicUsize = AtomicUsize::new(0);
    let base = neutral_base::neutral_base().join(format!(
        "assigned-pathspec-fp-{tag}-{}",
        N.fetch_add(1, Ordering::SeqCst)
    ));
    let _ = std::fs::remove_dir_all(&base);
    std::fs::create_dir_all(base.join("home").join(".cache")).unwrap();
    std::fs::create_dir_all(base.join("store")).unwrap();
    let proj = base.join("src").join("proj");
    std::fs::create_dir_all(proj.join("tests")).unwrap();
    std::fs::write(proj.join("README"), "x\n").unwrap();
    git(&proj, &["init", "-q"]);
    git(&proj, &["add", "-A"]);
    git(&proj, &["commit", "-q", "-m", "init"]);
    std::fs::write(proj.join("tests/new_fp.rs"), "// x\n").unwrap();
    (Fx { base }, proj)
}

fn decision(fx: &Fx, cwd: &Path, command: &str, subagent: bool) -> (String, String) {
    let mut p = serde_json::json!({
        "session_id": "assigned-pathspec-fp-20261118",
        "cwd": cwd,
        "hook_event_name": "PreToolUse",
        "tool_name": "Bash",
        "tool_input": {"command": command},
    });
    if subagent {
        p["agent_id"] = serde_json::json!("test-subagent");
    }
    let mut cmd = Command::new(env!("CARGO_BIN_EXE_blastguard"));
    cmd.env_clear()
        .env("HOME", fx.base.join("home"))
        .env("PATH", "/usr/bin:/bin")
        .env("CLAUDECODE", "1")
        .env("CLAUDE_CODE_ENTRYPOINT", "cli")
        .env("CLAUDE_PROJECT_DIR", cwd)
        .env("BLASTGUARD_APPROVALS_DIR", fx.base.join("store"))
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .current_dir(cwd)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let out = bounded::run_bounded(&mut cmd, p.to_string().as_bytes(), bounded::HOOK_DEADLINE);
    assert!(out.status.success(), "hook exited {:?}", out.status);
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

fn expect_allow(command: &str) {
    let (fx, proj) = repo("allow");
    for subagent in [false, true] {
        let (d, r) = decision(&fx, &proj, command, subagent);
        assert_eq!(d, "allow", "subagent={subagent}: {command:?} got {d} ({r})");
    }
}

/// Two assigned literals (the live relay command had `$a $b`).
// test-weakening-justified: ignore-added on a new RED for an unfixed FP (backlog 2026-11-18)
#[test]
#[ignore = "RED: 2026-11-18 blastguard FP — `a=lit && b=lit && git add -- $a $b` not resolved statically; observed main Ask"]
fn two_assigned_literal_pathspecs_git_add_allows() {
    expect_allow("a=tests/new_fp.rs && b=README && git add -- $a $b");
}

/// One assigned literal after a literal `cd` to the project.
// test-weakening-justified: ignore-added on a new RED for an unfixed FP (backlog 2026-11-18)
#[test]
#[ignore = "RED: 2026-11-18 blastguard FP — `cd /abs && a=lit && git add -- $a` not resolved statically; observed main Ask"]
fn assigned_literal_pathspec_after_cd_git_add_allows() {
    let (fx, proj) = repo("cd");
    let command = format!(
        "cd {} && a=tests/new_fp.rs && git add -- $a",
        proj.display()
    );
    for subagent in [false, true] {
        let (d, r) = decision(&fx, &proj, &command, subagent);
        assert_eq!(d, "allow", "subagent={subagent}: {command:?} got {d} ({r})");
    }
}

/// Control: one assigned literal, no `cd`, is already resolved.
#[test]
fn one_assigned_literal_pathspec_git_add_allows() {
    expect_allow("a=tests/new_fp.rs && git add -- $a");
}

/// Control: the literal twin.
#[test]
fn literal_pathspec_git_add_allows() {
    expect_allow("git add -- tests/new_fp.rs");
}

/// Control: an unbound `$a` may split / glob: not Allow.
#[test]
fn unbound_pathspec_git_add_not_allowed() {
    let (fx, proj) = repo("unbound");
    let (d, r) = decision(&fx, &proj, "git add -- $a", false);
    assert_ne!(d, "allow", "unbound $a was allowed ({r})");
}
